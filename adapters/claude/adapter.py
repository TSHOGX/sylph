#!/usr/bin/env python3
"""Claude adapter for `sylph.agent.v3`.

Translates between Sylph's normalized duplex protocol and Claude Code's own
bidirectional control protocol. `docs/session-protocol.md` governs the Sylph
side; the Claude side is `claude -p --input-format stream-json
--output-format stream-json`, which carries far more than the v1 runner contract
could express.

What this adapter exists to reach, none of it available through a one-shot pipe:

  * `initialize` returns the project's **real** slash commands (46 on a typical
    checkout here, against the one or two a v1 runner hand-declares), plus
    subagent definitions and the current permission mode.
  * `can_use_tool` arrives as a request and blocks until answered, which is what
    makes an approval prompt possible at all.
  * `interrupt` ends a turn and leaves the session alive.
  * `--forward-subagent-text` tags subagent output with `parent_tool_use_id`, so
    a subagent renders as a subagent rather than as an opaque Task tool call.

The single most important implementation rule: **stdin to `claude` is never
closed while the session lives.** Closing it is what v1 did to signal end-of-
prompt, and it aborts any in-flight permission request with
`AbortError: Stream closed`.
"""
import json
import os
import subprocess
import sys
import threading
import uuid

PROTOCOL = "sylph.agent.v3"
ADAPTER_VERSION = "3.0.0"

# What this adapter can do, from the protocol's closed vocabulary. Declared
# rather than assumed: the server refuses to send anything absent from here, so
# an honest list is what keeps the UI from offering a control that does nothing.
SUPPORTS = [
    "interrupt",
    "steer",
    "approval",
    "subagent",
    "slash",
    "set_config",
    "context",
    "fork",
]

# Spawn-time options, for the create-a-session screen, which has no live session to
# ask. Anything adjustable afterwards is a config option instead (`config_from`), and
# `model` deliberately appears in both: it is choosable up front and changeable later.
#
# `model` lists no choices because the real catalogue depends on the account and is
# only knowable once the CLI has loaded — so an empty list here means "free-form,
# ask the session", not "no models".
FLAGS = {
    "model": {
        "choices": [],
        "description": "Model for this session.",
    },
    "effort": {
        "choices": ["low", "medium", "high", "xhigh", "max"],
        "description": "Reasoning effort.",
    },
    "mode": {
        "choices": ["default", "acceptEdits", "plan", "manual", "dontAsk", "bypassPermissions"],
        "description": "Permission mode.",
    },
}

# Dialog kinds this adapter can render, declared at `initialize`.
#
# This declaration is load-bearing, and its absence was a real bug: the CLI's own
# schema says it "treats ABSENCE as 'cannot display' and fails closed: without the
# kind declared here, a dialog-gated flow degrades". So an undeclared client gets
# prose where a TUI gets a picker — `/plan` returns a paragraph instead of an
# approve/reject choice, and AskUserQuestion returns a question instead of options.
#
# Every kind here must actually be answerable through `resolve`. Declaring one we
# cannot render is worse than declaring none: the CLI would park a dialog waiting
# on a reply no UI will produce, which blocks the turn until its deadline.
SUPPORTED_DIALOG_KINDS = [
    # The interactive surfaces that make a session feel native.
    "permission_ask_user_question",   # AskUserQuestion: pick among options
    "permission_enter_plan_mode",     # a plan to approve or reject
    # Tool permissions that carry richer payloads than `can_use_tool`.
    "permission_bash",
    "permission_file",
    "permission_webfetch",
    "permission_skill",
    "permission_prompt",
    "permission_workflow",
    # Consent gates a user must be able to answer, or the turn stalls.
    "cost_threshold",
    "sandbox_network_access",
    "refusal_fallback_prompt",
    "mcp_url_elicitation",
]

# Claude permission modes that route decisions to us rather than deciding alone.
# `bypassPermissions` auto-approves before any callback runs, so an approval
# prompt under it would never fire — worth knowing rather than discovering.
INTERACTIVE_MODES = {"default", "acceptEdits", "plan", "manual"}

class UnknownConfig(Exception):
    """A config id this adapter cannot set.

    Its own exception type so the `set_config` handler can answer
    `UNSUPPORTED_FIELD` — a capability answer a UI can disable a control over —
    rather than a generic adapter error a client can only retry.
    """


out_lock = threading.Lock()


def emit(obj):
    """Write one NDJSON line to the server. Serialized: two threads emit here."""
    with out_lock:
        sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n")
        sys.stdout.flush()


def log(message):
    sys.stderr.write(str(message) + "\n")
    sys.stderr.flush()


def ok(request_id, **fields):
    emit({"t": "ok", "id": request_id, **fields})


def err(request_id, code, message):
    emit({"t": "err", "id": request_id, "code": code, "message": message})


def event(kind, phase=None, **fields):
    body = {"t": "event", "kind": kind}
    if phase:
        body["phase"] = phase
    body.update({k: v for k, v in fields.items() if v is not None})
    emit(body)


def soul_path():
    """The identity file, or "" for none.

    `SYLPH_SOUL` set to the empty string means inject nothing — the protocol
    reserves it for that, and a native session depends on it. Unset means resolve
    the conventional location, which keeps this adapter usable outside Sylph Web.
    """
    override = os.environ.get("SYLPH_SOUL")
    if override is not None:
        return override if os.path.isfile(override) else ""
    here = os.path.dirname(os.path.abspath(__file__))
    default = os.path.normpath(os.path.join(here, "..", "..", "SOUL.md"))
    return default if os.path.isfile(default) else ""


def form_answer_of(user_input):
    """The `{values, notes}` a client sent on `resolve`, defensively.

    The server validates a form answer against the declared form, so a well-behaved
    client's payload is already the right shape. This still tolerates a missing or
    wrongly-typed part rather than raising: an exception here would leave the turn
    parked until the CLI's own deadline, which reads as the agent hanging, and a
    partial answer is worth forwarding over none.
    """
    if not isinstance(user_input, dict):
        return {}, {}
    values = user_input.get("values")
    notes = user_input.get("notes")
    return (
        values if isinstance(values, dict) else {},
        notes if isinstance(notes, dict) else {},
    )


class Session:
    """One `claude` process and the translation around it."""

    def __init__(self):
        self.proc = None
        self.native_session_id = None
        self.pending_control = {}       # our control id -> threading.Event box
        self.tool_names = {}            # tool_use_id -> tool name, for results
        # (trajectory, content block index) -> in-flight block.
        #
        # Keyed by trajectory and not by index alone, which was a correctness bug
        # rather than a tidiness one: claude numbers content blocks per message, so
        # two concurrent subagents both open index 0, and one index-keyed map made
        # each one's deltas append to the other's text. Invisible until two
        # delegates run at once, and then it is corrupted output, not a layout
        # glitch.
        self.blocks = {}
        self.subagent_tools = set()     # tool_use_ids that are subagent spawns
        # tool_use_id -> the trajectory the spawning Task call itself ran in, so a
        # nested delegation closes back into its parent rather than at the root.
        self.subagent_parent = {}
        # task_id -> trajectory id, for a host that multiplexes.
        #
        # `Workflow` fans out to N agents under one `tool_use_id`, so keying a
        # trajectory by the host collapsed all N into one row where the last name to
        # arrive won: a two-agent workflow rendered as a single card reading
        # "Demo: agent-2", and the other agent was simply absent. The vendor's
        # per-delegate identity is `task_id`, so that is what a trajectory is keyed
        # by, with the host as its `span`. Same rule as `self.blocks` above, one
        # level up: never key a trajectory by something the vendor multiplexes.
        self.task_trajectories = {}
        # Trajectory ids closed already, so a late notification cannot reopen one.
        self.closed_trajectories = set()
        # host tool_use_id -> the task ids running under it. A host with exactly one
        # delegate lets forwarded output tagged with the *host* be attributed to the
        # delegate that actually produced it; with several, claude forwards no
        # interior at all, so there is nothing to attribute.
        self.host_tasks = {}
        # task id -> its host, the inverse of the above. `task_updated` carries no
        # `tool_use_id`, so a close would otherwise land at the root while the
        # trajectory it closes sits inside a container — leaving the container open
        # forever, which is the same spinner one layer up.
        self.task_hosts = {}
        # task id -> its `task_type`, remembered from `task_started`.
        #
        # Only `task_started` carries it, and it is what separates a delegation
        # (`local_agent`) from a backgrounded shell job (`local_bash`) reporting
        # through the same notifications. Without the memo a close is unclassifiable
        # and re-opens the node its start was filtered out of.
        self.task_kinds = {}
        # task id -> whether the vendor said it runs in the background.
        #
        # Reported only by `task_started`, like `task_type`, and it changes what the
        # server should do when the turn ends: backgrounded work is *expected* to
        # outlive its turn, so closing it as abandoned records a contradiction that its
        # own later completion cannot undo.
        self.task_backgrounded = {}
        self.message_id = "claude"
        self.message_seq = 0
        self.turn_open = False
        # The last `initialize` response, kept so a config facet can be rebuilt
        # without another round trip to the CLI. Effort levels depend on the selected
        # model, so recomputing needs the catalogue, not just the chosen value.
        self.initialize_response = {}
        self.current_model = None
        self.current_effort = None
        # vendor request id -> 'can_use_tool' | 'dialog'. A permission and a dialog
        # take different response shapes, and guessing wrong parks the turn.
        self.request_kinds = {}
        # vendor request id -> the AskUserQuestion question texts, in the order they
        # were forwarded. The text is the CLI's own key into `answers`, so answering
        # needs the index-to-text map back.
        self.question_texts = {}
        # vendor request id -> the AskUserQuestion input, so the answers can be
        # written back into the shape the tool expects.
        self.question_inputs = {}
        self.lock = threading.Lock()

    # ---- vendor process -------------------------------------------------

    def start(self, workdir, session_id, flags, permission_mode):
        argv = [
            "claude", "-p",
            "--input-format", "stream-json",
            "--output-format", "stream-json",
            "--include-partial-messages",
            "--forward-subagent-text",
            "--verbose",
        ]

        mode = permission_mode or flags.get("mode") or "default"
        argv += ["--permission-mode", mode]
        # Routing permission decisions to us over stdio is the whole point; under
        # a bypassing mode Claude decides before any callback, so asking for the
        # channel there would promise a prompt that never arrives.
        if mode in INTERACTIVE_MODES:
            argv += ["--permission-prompt-tool", "stdio"]

        # An explicit settings file, when the caller has one. Sylph itself does
        # not set this; it exists because a caller sometimes has to pin
        # permission rules for a session — which is also how `verify.py` forces a
        # real approval instead of hoping the ambient rules withhold one.
        settings = os.environ.get("SYLPH_CLAUDE_SETTINGS")
        if settings and os.path.isfile(settings):
            argv += ["--settings", settings]

        soul = soul_path()
        if soul:
            # Additive, never replacing: Sylph is a voice layered over a working
            # coding agent, so the agent's own rules and the project's CLAUDE.md
            # must survive.
            argv += ["--append-system-prompt-file", soul]

        if session_id:
            argv += ["-r", session_id]
            # Recorded now, not discovered later. `open` must answer with the id, and
            # on this path the only other source is claude's first message — which has
            # not arrived when the reply is due. Leaving it unset made a resumed link
            # answer `sessionId: null`, which the server rejects as an unresumable
            # session ("Adapter opened a session without reporting its id"). The
            # effect was that every session became permanently unlinkable once its
            # first link died — after the 30-minute idle reaper, a server restart, or
            # an explicit detach — which is the ordinary path, not an error path.
            self.native_session_id = session_id
        else:
            # Assigning the id up front rather than discovering it means the
            # server can correlate with TraceDB before the first event lands.
            self.native_session_id = str(uuid.uuid4())
            argv += ["--session-id", self.native_session_id]

        if flags.get("model"):
            argv += ["--model", flags["model"]]
        if flags.get("effort"):
            argv += ["--effort", flags["effort"]]

        self.proc = subprocess.Popen(
            argv, cwd=workdir,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=None,
            text=True, encoding="utf-8", errors="replace", bufsize=1,
        )
        threading.Thread(target=self._read_vendor, daemon=True).start()

    def to_claude(self, obj):
        """Write to claude's stdin, which stays open for the session's life."""
        if not self.proc or self.proc.poll() is not None:
            raise RuntimeError("claude is not running")
        with self.lock:
            self.proc.stdin.write(json.dumps(obj, ensure_ascii=False) + "\n")
            self.proc.stdin.flush()

    def control(self, subtype, **fields):
        """Send a control_request and wait for its response."""
        request_id = "sylph-{}".format(uuid.uuid4().hex[:12])
        box = {"event": threading.Event(), "response": None}
        self.pending_control[request_id] = box
        self.to_claude({
            "type": "control_request",
            "request_id": request_id,
            "request": {"subtype": subtype, **fields},
        })
        if not box["event"].wait(timeout=30):
            self.pending_control.pop(request_id, None)
            raise RuntimeError("claude did not answer {} in time".format(subtype))
        return box["response"] or {}

    def apply_config(self, config_id, value):
        """Set one config option on the live session.

        The mapping from a Sylph config id to a vendor control lives here because it
        is a vendor fact: `permissionMode` is `set_permission_mode` for claude and
        does not exist at all for pi.
        """
        if config_id == "model":
            # `default`, null and an omitted model all reset to the session default,
            # per the CLI's own schema, so an explicit reset is passed through rather
            # than filtered out.
            self.control("set_model", model=value)
            self.current_model = value
        elif config_id == "effort":
            # Measured against the real CLI rather than inferred from the name:
            # `apply_flag_settings` refuses a bare field with "requires `settings` to
            # be an object", and `set_max_thinking_tokens` wants an integer budget,
            # not an effort name. Two wrong guesses, both of which would have failed
            # silently as a setting that never took.
            self.control("apply_flag_settings", settings={"effort": value})
            self.current_effort = value
        elif config_id == "fastMode":
            self.control("apply_flag_settings", settings={"fastMode": bool(value)})
        elif config_id == "permissionMode":
            self.control("set_permission_mode", mode=value)
            if self.initialize_response:
                self.initialize_response["current_permission_mode"] = value
        elif config_id == "cwd":
            self.control("set_cwd", cwd=value)
        else:
            raise UnknownConfig(config_id)

    def answer(self, vendor_id, option, user_input):
        """Reply to a vendor request in the shape its kind expects.

        A `can_use_tool` request wants `{behavior}`; a `request_user_dialog` wants
        a payload shaped by its dialog kind. Sending the wrong one is not a
        validation error the CLI reports — it parks the dialog and the turn stalls
        until its deadline, which reads as the agent hanging. So the kind each
        request arrived as is remembered rather than inferred from the answer.
        """
        kind = self.request_kinds.pop(vendor_id, "can_use_tool")
        allow = option in ("allow", "allow_always")

        if kind == "question":
            texts = self.question_texts.pop(vendor_id, [])
            base = dict(self.question_inputs.pop(vendor_id, {}) or {})

            if option in ("deny", "cancel"):
                # Declining to answer is a denied tool call, which the agent
                # handles as "the user did not pick one" rather than as an error.
                # `message`, never `feedback`: measured against claude 2.1.238, a
                # `feedback` key here is rejected as a malformed permission result
                # and the model is told the callback is broken — so the user's
                # refusal is reported as an integration bug.
                self.to_claude({
                    "type": "control_response",
                    "response": {"subtype": "success", "request_id": vendor_id,
                                 "response": {"behavior": "deny",
                                              "message": "The user declined to answer."}},
                })
                return

            # The answers, keyed by question *text*. This is the CLI's own key: it
            # builds the tool result by reading `answers[question]`, and a reply that
            # does not populate it is reported to the model as "The user did not
            # answer the questions" — which is exactly the bug this replaced. The
            # previous shape wrote `questions[0].selectedOptions`, a key that does not
            # occur anywhere in the CLI binary, so a correct pick was silently lost.
            values, notes = form_answer_of(user_input)
            answers = {}
            annotations = {}
            for index, text in enumerate(texts):
                value = values.get("q:{}".format(index))
                if isinstance(value, list):
                    # Multi-select arrives as a list and is delivered comma-joined,
                    # which is what the tool's output schema documents and what its
                    # own parser reads back. The flattening happens here because it
                    # is a vendor detail; the protocol carries a list.
                    picked = ", ".join(str(item) for item in value if str(item).strip())
                elif value is None:
                    picked = ""
                else:
                    picked = str(value)
                if picked:
                    answers[text] = picked
                note = (notes.get("q:{}".format(index)) or "").strip()
                if note:
                    annotations[text] = {"notes": note}

            updated = dict(base)
            updated["answers"] = answers
            if annotations:
                updated["annotations"] = annotations
            if not answers and not annotations:
                # Submitted with nothing filled in. Sent as `response` rather than as
                # empty answers, because the CLI reads that as "the user replied in
                # prose" instead of "the user answered nothing".
                updated["response"] = "The user submitted no answer."

            self.to_claude({
                "type": "control_response",
                "response": {"subtype": "success", "request_id": vendor_id,
                             "response": {"behavior": "allow",
                                          "updatedInput": updated}},
            })
            return

        if kind == "can_use_tool":
            if allow:
                body = {"behavior": "allow"}
                if user_input is not None:
                    body["updatedInput"] = user_input
                if option == "allow_always":
                    # A lasting grant, which is a policy change rather than one
                    # decision — hence a separate option in the UI.
                    body["updatedPermissions"] = [{"behavior": "allow"}]
            else:
                body = {"behavior": "deny",
                        "message": "The user declined this request."}
            self.to_claude({
                "type": "control_response",
                "response": {"subtype": "success", "request_id": vendor_id,
                             "response": body},
            })
            return

        # A dialog. `cancelled` is the CLI's own spelling for "the user declined",
        # and it is distinct from a permission's `deny`: the flow was abandoned
        # rather than refused, so Claude continues differently.
        if not allow and option not in ("submit",):
            body = {"behavior": "cancelled"}
        else:
            body = {"behavior": "completed"}
            if user_input is not None:
                # A form answer is unwrapped to the values the dialog asked for. The
                # `{values, notes}` envelope is Sylph's transport, not a shape any
                # vendor dialog expects back.
                values, _ = form_answer_of(user_input)
                body["result"] = values or user_input

        self.to_claude({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": vendor_id,
                         "response": body},
        })

    # ---- vendor -> sylph ------------------------------------------------

    def _read_vendor(self):
        for raw in self.proc.stdout:
            raw = raw.strip()
            if not raw:
                continue
            try:
                msg = json.loads(raw)
            except Exception:
                continue
            try:
                self._translate(msg)
            except Exception as error:          # never let one line kill the loop
                log("translate failed: {}".format(error))

        code = self.proc.wait()
        event("status", "completed",
              text="claude exited",
              data={"exitCode": code})

    def _translate(self, msg):
        if msg.get("session_id") and not self.native_session_id:
            self.native_session_id = msg["session_id"]

        mtype = msg.get("type")

        if mtype == "control_response":
            response = msg.get("response") or {}
            box = self.pending_control.pop(response.get("request_id"), None)
            if box:
                box["response"] = response.get("response") or {}
                box["event"].set()
            return

        if mtype == "control_request":
            self._vendor_request(msg)
            return

        if mtype == "stream_event":
            # `parent_tool_use_id` lives on the *envelope*, not the nested event,
            # and it is the only thing distinguishing a subagent's output from the
            # parent's own. Dropping it here — which this adapter did — published
            # every delegate's thinking, prose, and tool calls as the root agent's,
            # and the timeline had no way to tell them apart.
            self._stream_event(msg.get("event") or {},
                               msg.get("parent_tool_use_id"))
            return

        if mtype == "user":
            self._tool_results(msg)
            return

        if mtype == "system":
            self._system(msg)
            return

        if mtype == "result":
            usage = msg.get("usage")
            if usage:
                event("usage", "completed", streamId="claude-usage", data=usage)
            self.turn_open = False
            event("turn", "completed",
                  text=msg.get("result") or "",
                  isError=bool(msg.get("is_error")),
                  data={
                      "stopReason": msg.get("subtype"),
                      "durationMs": msg.get("duration_ms"),
                      "costUsd": msg.get("total_cost_usd"),
                  })

    def _vendor_request(self, msg):
        """A request from claude that a human must answer.

        Forwarded to the server and left pending. Answering it here — even with
        the safe-looking default — would silently remove the user from the
        decision this whole channel exists to give them.
        """
        request = msg.get("request") or {}
        subtype = request.get("subtype")
        vendor_id = msg.get("request_id")

        if os.environ.get("SYLPH_ADAPTER_DEBUG"):
            log("VENDOR REQUEST {} {}".format(
                subtype, json.dumps(request)[:600]))

        if subtype == "can_use_tool":
            self.request_kinds[vendor_id] = "can_use_tool"
            tool = request.get("tool_name")

            # AskUserQuestion is a permission request in form and a question in
            # substance: its input carries the agent's own options, and answering
            # it "allow" merely lets the agent ask — which is what made a TUI's
            # picker come back as a paragraph here. Verified against the real CLI;
            # there is no separate dialog for this.
            if tool == "AskUserQuestion":
                self._forward_question(vendor_id, request)
                return

            # Whether a lasting grant may be offered at all. The CLI marks asks
            # where accepting "always" would write a rule *broader than this ask*,
            # and its schema is explicit that a host "should omit any persistent-rule
            # affordance when set". Offering it anyway turns a narrow yes into a wide
            # one the user never read, so the option is withheld rather than merely
            # labelled differently.
            allow_persistent = not request.get("suppressAlwaysAllowRule")

            options = [{"id": "allow", "label": "Allow once"}]
            if allow_persistent:
                options.append({
                    "id": "allow_always",
                    "label": "Always allow {}".format(tool or "this"),
                    "persistent": True,
                })
            options.append({"id": "deny", "label": "Deny"})

            emit({
                "t": "request",
                "id": vendor_id,
                "kind": "approval",
                "toolUseId": request.get("tool_use_id"),
                "tool": tool,
                "title": request.get("description")
                         or "Allow {}?".format(request.get("display_name") or tool),
                "detail": request.get("input"),
                "options": options,
                "default": "deny",
                "risk": self._risk(tool, request.get("input")),
                "allowPersistent": allow_persistent,
            })
            return

        if subtype == "request_user_dialog":
            self._forward_dialog(vendor_id, request)
            return

        # Anything else is acknowledged so claude is never left blocking on a
        # subtype this adapter does not model.
        log("unhandled vendor request subtype: {}".format(subtype))
        self.to_claude({
            "type": "control_response",
            "response": {"subtype": "error", "request_id": vendor_id,
                         "error": "unsupported by the Sylph adapter"},
        })

    def _forward_question(self, vendor_id, request):
        """Turn an AskUserQuestion tool call into a form the user can fill in.

        Every question is forwarded, as one `select` field each. Sending only the
        first was a measurable loss rather than a simplification: answering all of
        them is reported to the model as "Your questions have been answered … you can
        now continue with these answers in mind", while answering one of two arrives
        as the weaker "The user answered …". The tool caps a call at four questions,
        so the panel stays finite.

        Field ids are positional (`q:0`), and `question_texts` keeps the index-to-text
        map. The text is the CLI's own key into `answers`, so it has to survive the
        round trip — but sending it as the field id would put a whole sentence, and
        whatever punctuation it contains, into an identifier a client echoes back.
        """
        payload = request.get("input") or {}
        questions = payload.get("questions") or []

        fields = []
        texts = []
        for index, question in enumerate(questions):
            if not isinstance(question, dict):
                continue
            text = question.get("question") or "Question {}".format(index + 1)
            texts.append(text)
            multi = bool(question.get("multiSelect"))

            choices = []
            for option in question.get("options") or []:
                if isinstance(option, dict):
                    label = option.get("label") or ""
                    description = option.get("description")
                else:
                    label, description = str(option), None
                if not label:
                    continue
                # `value` is the label, because the label is what the CLI matches on
                # when it builds the tool result — it never sent us an id.
                choice = {"value": label}
                if description:
                    choice["description"] = description
                choices.append(choice)

            field = {
                "type": "select",
                "id": "q:{}".format(index),
                "label": text,
                "mode": "many" if multi else "one",
                "choices": choices,
                # The CLI's own dialog always offers an "Other" row and treats what
                # is typed there as the answer. Withholding it here would make this
                # client strictly less able to answer than the terminal.
                "allowOther": True,
            }
            if question.get("header"):
                field["description"] = question["header"]
            if not multi:
                # Notes ride in `annotations`, which the CLI reads alongside the pick
                # rather than instead of it. Offered only on a single-select, matching
                # the terminal, where a note qualifies one chosen option.
                field["notes"] = {}
            fields.append(field)

        self.question_texts[vendor_id] = texts
        self.question_inputs[vendor_id] = payload
        # Answered through the permission path, because that is what it is on the
        # wire: the answers are written into the tool input.
        self.request_kinds[vendor_id] = "question"

        emit({
            "t": "request",
            "id": vendor_id,
            "kind": "choice",
            "tool": "AskUserQuestion",
            "toolUseId": request.get("tool_use_id"),
            "title": (texts[0] if len(texts) == 1 else None)
                     or "Claude has a question",
            "detail": {"questions": questions},
            "form": {"fields": fields},
            # Verbs, not answers: the form carries what was chosen, so an option is
            # what to do with it. Declining is still offered — the user must be able
            # to refuse to answer, which the agent handles as a denied tool call.
            "options": [
                {"id": "submit", "label": "Send answer"},
                {"id": "deny", "label": "Don't answer"},
            ],
            "default": "deny",
            "risk": "safe",
        })

    def _forward_dialog(self, vendor_id, request):
        """Project one `request_user_dialog` onto Sylph's request shape.

        The payload differs per `dialog_kind`, and the options are what make the
        difference between a picker and a paragraph. A kind whose payload we cannot
        read still forwards as a two-option accept/reject rather than being
        dropped, because the turn is blocked either way and a coarse choice beats
        no choice.
        """
        kind = request.get("dialog_kind") or ""
        payload = request.get("payload") or {}
        self.request_kinds[vendor_id] = "dialog"

        # Note there is deliberately no `permission_ask_user_question` branch here.
        # The kind is declared at `initialize` — the declaration is what stops the CLI
        # degrading the flow to prose — but measured against claude 2.1.238 the ask
        # arrives as a `can_use_tool` for the AskUserQuestion tool, carrying
        # `requires_user_interaction: true`, and never as a `request_user_dialog`. A
        # branch here was dead code that also answered in the wrong shape, and a
        # plausible-looking dead branch is worse than none: it reads as the path being
        # handled.

        if kind == "permission_enter_plan_mode":
            emit({
                "t": "request",
                "id": vendor_id,
                "kind": "dialog",
                "title": "Approve this plan?",
                # The plan text is the thing being decided on, so it goes where a
                # client renders content rather than into a label.
                "detail": {"plan": payload.get("plan") or payload.get("message") or ""},
                "options": [
                    {"id": "allow", "label": "Approve"},
                    {"id": "allow_always", "label": "Approve and auto-accept edits"},
                    {"id": "deny", "label": "Keep planning"},
                ],
                "default": "deny",
            })
            return

        if kind == "cost_threshold":
            emit({
                "t": "request",
                "id": vendor_id,
                "kind": "dialog",
                "title": "Continue past the spending threshold?",
                "detail": payload,
                "options": [
                    {"id": "allow", "label": "Continue"},
                    {"id": "deny", "label": "Stop here"},
                ],
                "default": "deny",
            })
            return

        # A permission-shaped dialog carries the tool call it is gating.
        if kind.startswith("permission_"):
            emit({
                "t": "request",
                "id": vendor_id,
                "kind": "approval",
                "tool": payload.get("tool_name") or kind.removeprefix("permission_"),
                "toolUseId": request.get("tool_use_id"),
                "title": payload.get("title")
                         or payload.get("message")
                         or "Allow this action?",
                "detail": payload.get("input") or payload,
                "options": [
                    {"id": "allow", "label": "Allow once"},
                    {"id": "allow_always", "label": "Always allow", "persistent": True},
                    {"id": "deny", "label": "Deny"},
                ],
                "default": "deny",
                "risk": self._risk(payload.get("tool_name"), payload.get("input")),
            })
            return

        # Unrecognized kind. Still answerable, because a parked dialog blocks the
        # turn until its deadline and a coarse choice beats silence.
        emit({
            "t": "request",
            "id": vendor_id,
            "kind": "dialog",
            "title": payload.get("title") or payload.get("message") or "Claude asks",
            "detail": {"dialogKind": kind, **payload},
            "options": [
                {"id": "allow", "label": "Continue"},
                {"id": "deny", "label": "Cancel"},
            ],
            "default": "deny",
        })

    @staticmethod
    def _risk(tool, tool_input):
        """Advisory emphasis only. Deliberately crude: a real risk model belongs
        in the agent's own permission rules, not in a display hint."""
        if tool in ("Write", "Edit", "NotebookEdit"):
            return "destructive"
        if tool in ("WebFetch", "WebSearch"):
            return "network"
        if tool == "Bash":
            command = (tool_input or {}).get("command", "")
            if any(token in command for token in ("rm ", "git push", "curl ", "sudo ")):
                return "destructive"
        return "safe"

    def _stream_event(self, nested, parent=None):
        """Claude's partial-message stream, projected onto Sylph event kinds.

        `parent` is the envelope's `parent_tool_use_id`: the delegating call this
        output came from, or None for the root agent's own work. It becomes the
        event's `span`, which is what lets the timeline nest a delegation to any
        depth (`docs/session-protocol.md` § Delegated trajectories).
        """
        parent = self._delegate_of(parent)
        ntype = nested.get("type")

        if ntype == "message_start":
            self.message_seq += 1
            self.message_id = (nested.get("message") or {}).get("id") \
                or "claude-{}".format(self.message_seq)
            # A subagent's message must not open the turn, and must not be counted
            # as the root's: only the root agent's stream bounds the turn.
            if not self.turn_open and parent is None:
                self.turn_open = True
                event("turn", "started")
            return

        if ntype == "content_block_start":
            index = nested.get("index", 0)
            content = nested.get("content_block") or {}
            kind = {
                "text": "assistant",
                "thinking": "thinking",
                "tool_use": "tool_call",
            }.get(content.get("type"))
            if not kind:
                return
            stream_id = content.get("id") or "{}:{}".format(self.message_id, index)
            name = content.get("name")
            text = content.get("text") or content.get("thinking") or ""

            if name:
                self.tool_names[stream_id] = name
                # `Task` is *declared* a trajectory here, and it is the only tool
                # that can be. claude forwards a Task delegate's own events under
                # the spawning id, so this block and the child's output share one
                # identity and no correlation table is needed — which also means
                # its `tool_result` is the delegated work's outcome rather than a
                # tool's return value, so it closes the trajectory below.
                #
                # Every *other* delegating tool is discovered rather than declared.
                # This used to be the only path, and it was a guess standing in for
                # a fact that arrives later: a tool call is known to have delegated
                # when a child arrives carrying its id, which is always after the
                # call was published. The name test was correct until the vendor
                # shipped `Agent` and `Workflow`, and an MCP server can add another
                # at any time — so the list could never be completed, only extended
                # after each report. Those are now promoted by the `span` their
                # children already carry (`docs/session-protocol.md` § Promotion),
                # which needs no list and covers tools nobody has written yet.
                if name == "Task":
                    self.subagent_tools.add(stream_id)
                    self.subagent_parent[stream_id] = parent
                    kind = "subagent"

            self.blocks[(parent, index)] = {
                "kind": kind, "id": stream_id, "name": name, "text": text,
                "span": parent,
            }
            event(kind, "started", streamId=stream_id, span=parent,
                  # A delegation's display name is the kind of agent, not "Task".
                  name=_task_label(content) if kind == "subagent" else name,
                  text=text or None)
            return

        if ntype == "content_block_delta":
            index = nested.get("index", 0)
            block = self.blocks.get((parent, index))
            if not block:
                return
            delta = nested.get("delta") or {}
            text = (delta.get("text") or delta.get("thinking")
                    or delta.get("partial_json") or "")
            if not text:
                return
            block["text"] += text
            # A trajectory's own partial JSON is its brief arriving a token at a
            # time, which is noise on a card that already names the agent. Its
            # `started` and `completed` carry everything worth showing.
            if block["kind"] == "subagent":
                return
            event(block["kind"], "delta",
                  streamId=block["id"], span=block["span"],
                  name=block["name"], text=text)
            return

        if ntype == "content_block_stop":
            index = nested.get("index", 0)
            block = self.blocks.pop((parent, index), None)
            if not block:
                return
            data = None
            if block["kind"] in ("tool_call", "subagent"):
                # Tool input arrives as streamed partial JSON, so it is only
                # parseable once the block is closed.
                try:
                    data = {"input": json.loads(block["text"])}
                except Exception:
                    data = {"input": block["text"]}
            if block["kind"] == "subagent":
                # The trajectory does not close here — the Task block closing only
                # means the *request* is fully stated. It closes when its result
                # arrives, which is when the delegated work actually finished. A
                # `delta` carries the brief across without claiming an outcome.
                #
                # It also carries the real name: the brief is streamed partial
                # JSON, so at `started` there was nothing to read it from, and this
                # is the first point where the agent's own type is known.
                label = _task_label(data or {})
                self.tool_names[block["id"]] = label
                event("subagent", "delta", streamId=block["id"],
                      span=block["span"], name=label, data=data)
                return
            event(block["kind"], "completed",
                  streamId=block["id"], span=block["span"],
                  name=block["name"], text=block["text"], data=data)

    def _tool_results(self, msg):
        message = msg.get("message") or {}
        content = message.get("content") or []
        if not isinstance(content, list):
            content = [content]

        # `--forward-subagent-text` tags a subagent's own output with the tool
        # call that spawned it. That tag is what lets the timeline nest it, and
        # `_delegate_of` resolves it to the delegate when the host is only a
        # container for one.
        parent = self._delegate_of(msg.get("parent_tool_use_id"))

        for block in content:
            if not isinstance(block, dict):
                continue
            if block.get("type") != "tool_result":
                continue
            tool_id = block.get("tool_use_id") or "claude-tool-result"

            if tool_id in self.subagent_tools:
                # The delegated work finished. This closes the trajectory, and its
                # span is the trajectory the *spawning* call ran in — not `parent`,
                # which for a nested Task is the child's own id. Getting that wrong
                # reparents a depth-2 delegation onto itself.
                event("subagent", "completed",
                      streamId=tool_id,
                      span=self.subagent_parent.get(tool_id),
                      name=self.tool_names.get(tool_id),
                      text=flatten(block.get("content")),
                      isError=bool(block.get("is_error")))
                continue

            event("tool_result", "completed",
                  streamId=tool_id,
                  # A delegate's own tool results are tagged with the Task that
                  # spawned it, which is precisely this event's trajectory.
                  span=parent,
                  name=self.tool_names.get(tool_id),
                  text=flatten(block.get("content")),
                  isError=bool(block.get("is_error")))

    def _system(self, msg):
        subtype = msg.get("subtype")

        if subtype == "init":
            # `init` arrives once per *turn*, not once per process — measured, two
            # user messages produce two of them. So this must speak about the
            # permission mode and nothing else.
            #
            # This is where the 46-commands-to-0 bug lived: it sent the whole
            # capability surface with only this one field set, and the server
            # assigned it. Naming the facet is what makes the rest survive.
            event("capabilities", facets={
                "permissionMode": msg.get("permissionMode"),
            })
            # The model can change without us asking — a fallback under load, or a
            # `/model` the user typed as text — so the surface is refreshed from what
            # the CLI now reports rather than from what we last set.
            if msg.get("model"):
                self.current_model = msg["model"]
            return

        if subtype == "commands_changed":
            # A skill was edited or a plugin loaded, so the composer's list is now
            # stale. Re-read it rather than trusting the notification's payload,
            # which carries only that something changed.
            try:
                fresh = self.control("initialize")
                self.initialize_response = fresh
                event("capabilities", facets=capabilities_from(
                    fresh, self.current_model, self.current_effort))
            except Exception as error:
                log("failed to refresh commands: {}".format(error))
            return

        if subtype == "compact_boundary":
            event("compact", "completed",
                  text="Context was compacted",
                  data=msg.get("compact_metadata"))
            return

        # Measured against claude 2.x with `--forward-subagent-text`: these four and
        # no others. `task_completed` and `task_failed` do not exist, and an earlier
        # version of this branch listed them — so nothing ever closed a delegate and
        # every workflow card span forever. Names guessed from a vendor's vocabulary
        # compile and then silently never fire; `/tmp/probe_task.py` printed one real
        # turn's payloads to settle it.
        if subtype in ("task_started", "task_progress", "task_updated",
                       "task_notification"):
            self._task_notification(subtype, msg)
            return

        if subtype in ("error", "api_error"):
            event("status", "completed",
                  text=str(msg.get("error") or msg.get("message") or "claude error"),
                  isError=True)
            return

        if subtype == "notification":
            event("notice", text=str(msg.get("message") or ""))

    def _delegate_of(self, host):
        """The trajectory that forwarded output tagged with `host` belongs to.

        Usually `host` itself. The exception is a host that announced a delegate by
        `task_id` — `Agent`, and anything else that reports through `task_*` — where
        the trajectory is the delegate and the host is only its container. claude
        still tags the forwarded output with the *host*, so without this the work
        lands on the container while the delegate's own node stays empty, and one
        delegation reads as two: a node holding everything and a sibling holding
        nothing. Measured against a real delegated turn.

        Only resolved when the host has exactly one delegate. With several, claude
        forwards no interior at all, so there is nothing to attribute and a guess
        would put one delegate's work under another — the corruption this whole
        change exists to prevent.
        """
        if not host:
            return host
        tasks = self.host_tasks.get(host)
        if tasks and len(tasks) == 1:
            return next(iter(tasks))
        return host

    def _task_notification(self, subtype, msg):
        """A delegate's lifecycle, reported by the host tool rather than streamed.

        `Task` forwards its delegate's own events, so its trajectory is opened by
        the tool block and filled by forwarded output. Every other delegating tool
        — `Agent`, `Workflow`, an MCP server's own — reports only these
        notifications, and they are then the *whole* record of the delegated work.

        Two things here are load-bearing.

        **The trajectory is keyed by `task_id`, not by the host's `tool_use_id`.**
        `Workflow` fans out to N agents under one tool call, so keying by the host
        collapsed all N into a single row where the last name to arrive won:
        measured on a real session, a two-agent workflow rendered as one card
        reading "Demo: agent-2" and the other agent was absent entirely. The host
        becomes the `span` instead, which promotion turns into their container
        (`docs/session-protocol.md` § Promotion).

        **A status line is a snapshot, so it goes in `data`.** These notifications
        re-send the whole current label rather than a suffix. Appending it as delta
        text stacked six copies of one sentence in the journal.

        The four subtypes, measured rather than assumed — every field name here comes
        from printing one real delegated turn:

            task_started       task_id, tool_use_id, description, subagent_type,
                               prompt, spawn_depth, task_type
            task_progress      task_id, tool_use_id, description, last_tool_name,
                               usage
            task_updated       task_id, patch{status, end_time} — and *no*
                               tool_use_id, which is why the host is remembered
            task_notification  task_id, tool_use_id, status, summary, output_file

        So a close arrives two ways: `task_updated` with a terminal `patch.status`,
        and `task_notification` with a terminal `status` plus the delegate's own
        `summary`. Whichever lands first closes the trajectory and the other is
        ignored, because reopening a closed one is what leaves a permanently running
        duplicate.
        """
        host = msg.get("tool_use_id")

        # A host that already declared itself a trajectory *is* the delegation —
        # `Task` publishes its block under this id and claude forwards the child's
        # own events under it too. Minting a second id from `task_id` there splits
        # one delegation into two nodes: the work lands on the declared id and the
        # minted one stays empty, and since nothing ever closes it, it reads as
        # running forever. Measured against a real delegated turn, which is what
        # `verify.py` caught.
        if host and host in self.subagent_tools:
            trajectory = host
        else:
            trajectory = msg.get("task_id") or host
        if not trajectory:
            # Nothing addressable. Reported as a plain notice rather than dropped,
            # because a delegation nobody can name still happened.
            event("notice", text=str(msg.get("summary") or subtype))
            return

        # A late notification must not reopen a closed trajectory: that would leave
        # a second, permanently running node under the same id, which is the
        # spinner this whole change exists to remove.
        if trajectory in self.closed_trajectories:
            return

        # Not every `task_*` notification describes a delegation.
        #
        # `run_in_background` on Bash reports through this exact channel, and
        # `task_type` is the only thing that distinguishes it: measured on claude 2.x,
        # `local_bash` for a backgrounded shell job against `local_agent` for a real
        # delegation. Treating both as trajectories published a *shell command* as a
        # subagent — one card named `find / -maxdepth 8 -ipath "*workflows*" …`, its
        # error flag escalating the Bash call that hosted it, so a turn that had done
        # nothing wrong read `failed` from its first moment. A tool call already
        # renders as a tool call; wrapping one in a delegation card adds a layer that
        # describes nothing.
        #
        # Remembered from `task_started`, because neither `task_updated` nor
        # `task_notification` carries `task_type` — so a close would otherwise be
        # unclassifiable and would open the very node its `started` was filtered out
        # of. Unknown types are *allowed* through: a vendor adding a delegating task
        # kind should nest by default, and the failure mode of guessing wrong here is
        # a missing subtree rather than a mislabelled one.
        task_type = msg.get("task_type")
        if task_type:
            self.task_kinds[trajectory] = task_type
        if msg.get("is_backgrounded"):
            self.task_backgrounded[trajectory] = True
        if self.task_kinds.get(trajectory) == "local_bash":
            return

        # Which delegates run under this host, recorded on the way in. `task_updated`
        # carries no `tool_use_id` at all, so a close cannot be placed in the tree
        # from its own payload — without this it would land at the root while the
        # trajectory it closes sits inside a container, and the container would then
        # never resolve.
        if host and host != trajectory:
            self.host_tasks.setdefault(host, set()).add(trajectory)
            self.task_hosts[trajectory] = host

        # The host's own trajectory, so a delegation inside a delegation closes back
        # into its parent rather than at the root. A host that is not itself a known
        # delegation is its own span, and promotion makes that a trajectory.
        span = (host if host and host != trajectory
                else self.task_hosts.get(trajectory)
                or self.subagent_parent.get(trajectory))

        # `description` is a live label, not a stable name: measured as "Read README.md
        # first heading" at `task_started` and "Reading README.md" during progress. The
        # later one is better informed, and the server keeps the latest a phase gave.
        name = msg.get("description") or msg.get("subagent_type")

        data = {k: v for k, v in msg.items()
                if k in ("task_id", "subagent_type", "tool_use_id",
                         "last_tool_name", "spawn_depth", "task_type",
                         "is_backgrounded")}
        # Carried on every phase, not only the `started` that reports it, because the
        # server reads it off whichever row it has. A delegation the vendor says is
        # backgrounded is *expected* to outlive its turn, so the turn-settle sweep must
        # not close it as abandoned — it will report its own outcome later, and a close
        # written before that arrives contradicts it permanently.
        if self.task_backgrounded.get(trajectory):
            data["is_backgrounded"] = True
        if name:
            data["progress"] = str(name)

        patch = msg.get("patch") if isinstance(msg.get("patch"), dict) else {}
        status = str(patch.get("status") or msg.get("status") or "")
        if status in ("completed", "failed", "cancelled", "error"):
            self.closed_trajectories.add(trajectory)
            self.task_trajectories.pop(trajectory, None)
            event("subagent", "completed",
                  streamId=trajectory, span=span, name=name,
                  # `summary` is the delegate's own answer, which only
                  # `task_notification` carries — so a close arriving via
                  # `task_updated` has no text, and that is honest rather than empty.
                  text=str(msg.get("summary") or ""),
                  isError=status in ("failed", "error"),
                  data=data)
            return

        # `started` only on first sighting. A second `started` under one id would
        # open a duplicate node, and these notifications arrive repeatedly for one
        # delegate — which is why progress is a `delta` however it was announced.
        if trajectory not in self.task_trajectories:
            self.task_trajectories[trajectory] = host
            event("subagent", "started",
                  streamId=trajectory, span=span,
                  name=name or "Subagent", data=data)
            return

        event("subagent", "delta",
              streamId=trajectory, span=span, name=name, data=data)


def _task_label(content):
    """What to call a delegation on screen.

    "Task" is the tool's name, not the work's: a timeline of six identical rows
    reading "Task" is unreadable. The input carries better, but it only arrives as
    streamed partial JSON, so at `started` there is usually nothing yet — the
    trajectory is renamed by the `delta` that carries its parsed brief. Falling
    back to "Subagent" rather than "Task" keeps the vendor's tool name out of the
    UI, since the server's vocabulary is trajectories.
    """
    payload = content.get("input")
    if isinstance(payload, dict):
        label = payload.get("subagent_type") or payload.get("description")
        if label:
            return str(label)
    return "Subagent"


def flatten(content):
    """Tool results arrive as a string, or as a list of content blocks."""
    if isinstance(content, str):
        return content
    if not isinstance(content, list):
        return json.dumps(content, ensure_ascii=False) if content is not None else ""
    parts = []
    for block in content:
        if isinstance(block, str):
            parts.append(block)
        elif isinstance(block, dict):
            parts.append(block.get("text") or block.get("content") or "")
    return "".join(str(part) for part in parts if part)


# Which of Claude's own builtins are session *control* rather than prompts, and the
# config option each one drives.
#
# This table lives in the adapter because classification is a vendor fact: `claude`
# returns `/model` in the same list as `/verify` with no field telling them apart,
# while `pi` already excludes its TUI commands and ACP has no notion of the split at
# all. A list like this in the server or the UI would encode one vendor's taxonomy as
# though it were universal, which is the mistake v3 exists to stop repeating.
#
# A command mapping to `None` is still control — it is a session operation rather
# than something to think about — but has no picker behind it, so a client runs it
# without offering choices.
# Only the ones that genuinely resolve to a picker. `/context`, `/compact`,
# `/usage` and `/clear` are deliberately *not* here: each one produces output worth
# keeping — a context table, a compaction boundary, a cost summary — so they belong
# in the transcript as turns. Naming them control would have hidden their answers in
# a dialog that closes.
#
# `/doctor`, `/init`, `/mcp` and `/agents` are likewise prompts: they do work and
# report on it. An earlier draft listed them here on the theory that "builtin"
# implied "control", and the observable effect was a set of commands that opened an
# empty picker instead of doing anything.
CONTROL_COMMANDS = {
    "model": "model",
    "fast": "fastMode",
}

# Builtins, for `source`. Everything the CLI reports arrives in one flat list, and
# v2 labelled all 46 as `skill` — untrue for `/model`, and the label is what a UI
# groups by, so the menu grouped builtins under Skills.
BUILTIN_SOURCES = set(CONTROL_COMMANDS) | {
    "review", "resume", "rewind", "todos", "help", "status", "cost", "memory",
    "export", "hooks", "plugin", "skill-doctor", "loop", "design-sync",
    "design-login", "artifacts", "tasks", "workflows",
}


def commands_from(initialize_response):
    """Project claude's command list onto Sylph's, classifying as it goes.

    On this checkout the CLI returns 46 entries — skills, project commands, plugins,
    and its own builtins — where a v1 descriptor carried whatever its author typed by
    hand.
    """
    commands = []
    for entry in initialize_response.get("commands") or []:
        if isinstance(entry, str):
            commands.append({"name": entry, "description": "", "kind": "prompt"})
            continue
        if not isinstance(entry, dict):
            continue
        name = entry.get("name")
        if not name:
            continue

        spec = {
            "name": name,
            "description": entry.get("description") or "",
            # `argumentHint` is the CLI's spelling. v2 also tried `argNames`, which
            # this version does not send; keeping both costs nothing and a wrong
            # guess here silently drops every hint.
            "argHint": entry.get("argumentHint") or entry.get("argNames") or None,
            "source": "builtin" if name in BUILTIN_SOURCES else "skill",
            # Carried because the terminal honours them: `/usage` answers to `cost`
            # and `stats`, `/clear` to `reset` and `new`.
            "aliases": entry.get("aliases") or [],
        }

        if name in CONTROL_COMMANDS:
            spec["kind"] = "control"
            config_id = CONTROL_COMMANDS[name]
            if config_id:
                spec["configId"] = config_id
        else:
            spec["kind"] = "prompt"

        commands.append({k: v for k, v in spec.items() if v is not None})
    return commands


def models_from(initialize_response):
    """The model catalogue, in the CLI's own spelling.

    Measured against `claude` 2.1.238: an entry is
    `{"value","resolvedModel","displayName","description","supportsEffort",
    "supportedEffortLevels",...}`. v2 read `id`/`label`, which exist on none of
    them, so the model list was empty on every session ever opened — and the
    composer's model control had nothing to show. Reading the wrong key failed
    silently, which is why this is spelled out here.
    """
    models = []
    for entry in initialize_response.get("models") or []:
        if isinstance(entry, str):
            models.append({"value": entry, "name": entry})
        elif isinstance(entry, dict) and entry.get("value"):
            models.append({
                "value": entry["value"],
                "name": entry.get("displayName") or entry["value"],
                "description": entry.get("description"),
                "effortLevels": entry.get("supportedEffortLevels") or [],
            })
    return models


def config_from(initialize_response, current_model=None, current_effort=None):
    """Everything adjustable on this live session, typed.

    Options are re-derived rather than cached because they are not independent: the
    effort levels a session can offer come from the *selected* model, so a model
    change can shrink or grow them. A flat session-global list of efforts — which is
    what v2's `flags.choices` was — is wrong for any agent whose models differ.
    """
    models = models_from(initialize_response)
    options = []

    selected = current_model or next(
        (m["value"] for m in models if m["value"] == "default"),
        models[0]["value"] if models else None,
    )

    if models:
        options.append({
            "type": "select",
            "id": "model",
            "name": "Model",
            "category": "model",
            "currentValue": selected,
            "options": [
                {k: v for k, v in {
                    "value": m["value"],
                    "name": m["name"],
                    "description": m.get("description"),
                }.items() if v is not None}
                for m in models
            ],
        })

    # Effort, scoped to the selected model. Omitted rather than shown empty when the
    # model does not support it: an empty picker invites a choice that cannot exist.
    levels = next(
        (m.get("effortLevels") or [] for m in models if m["value"] == selected),
        [],
    )
    if levels:
        options.append({
            "type": "select",
            "id": "effort",
            "name": "Reasoning effort",
            "category": "effort",
            "currentValue": current_effort,
            "options": [{"value": level, "name": level.capitalize()} for level in levels],
        })

    mode = initialize_response.get("current_permission_mode")
    options.append({
        "type": "select",
        "id": "permissionMode",
        "name": "Permissions",
        "category": "permission_mode",
        "currentValue": mode,
        "options": [
            {"value": choice, "name": choice}
            for choice in FLAGS["mode"]["choices"]
        ],
    })

    # Output style is deliberately *not* offered. The CLI reports
    # `available_output_styles`, but probing every plausible control subtype found
    # none that sets it, and an option a client can render but not apply is worse
    # than an absent one: the picker would accept a choice that silently does
    # nothing. It becomes a config option the day a setter is confirmed.

    if initialize_response.get("fast_mode_state") is not None:
        options.append({
            "type": "boolean",
            "id": "fastMode",
            "name": "Fast mode",
            "category": "speed",
            "currentValue": initialize_response.get("fast_mode_state") == "on",
        })

    return options


def capabilities_from(initialize_response, current_model=None, current_effort=None):
    """The full capability surface, as facets.

    Every caller sends facets, including this one. That is the point: there is no
    path left that can express "the whole surface" and therefore no path that can
    accidentally erase a facet it was not talking about.
    """
    subagents = []
    for entry in initialize_response.get("agents") or []:
        if isinstance(entry, dict) and entry.get("name"):
            subagents.append({
                "name": entry["name"],
                "description": entry.get("description") or "",
            })
        elif isinstance(entry, str):
            subagents.append({"name": entry, "description": ""})

    return {
        "commands": commands_from(initialize_response),
        "subagents": subagents,
        "config": config_from(initialize_response, current_model, current_effort),
        "permissionMode": initialize_response.get("current_permission_mode"),
    }


def claude_version():
    try:
        result = subprocess.run(["claude", "--version"], capture_output=True,
                                text=True, timeout=10)
        return result.stdout.strip().split()[0] if result.returncode == 0 else None
    except Exception:
        return None


def build_content(text, attachments):
    """One user message, with images inline and other files named by path.

    Images have to be base64 content blocks — the Read tool is unreliable on
    binary image data — while text and PDFs are better left to the CLI's own
    tools, which handle them well and do not bloat the message.
    """
    images, others = [], []
    for path in attachments or []:
        if not os.path.isfile(path):
            others.append(path)
            continue
        mime = image_mime(path)
        (images if mime else others).append((path, mime) if mime else path)

    if not images:
        return prefix_paths(others, text)

    blocks = []
    import base64
    for path, mime in images:
        try:
            with open(path, "rb") as handle:
                data = base64.standard_b64encode(handle.read()).decode()
        except OSError as error:
            log("skipping unreadable image {}: {}".format(path, error))
            continue
        blocks.append({"type": "image",
                       "source": {"type": "base64", "media_type": mime, "data": data}})
    body = prefix_paths(others, text)
    if body:
        blocks.append({"type": "text", "text": body})
    return blocks


def prefix_paths(paths, text):
    if not paths:
        return text
    lines = ["Files attached for this turn (absolute paths on local disk):"]
    lines += ["- " + str(path) for path in paths]
    lines += ["", text]
    return "\n".join(lines)


def image_mime(path):
    """Sniff the magic bytes. `mimetypes` misses enough real cases that the
    header is the more reliable signal, and a wrong guess here sends a binary
    blob as text."""
    try:
        with open(path, "rb") as handle:
            head = handle.read(12)
    except OSError:
        return None
    if head.startswith(b"\x89PNG\r\n\x1a\n"):
        return "image/png"
    if head[:3] == b"\xff\xd8\xff":
        return "image/jpeg"
    if head[:6] in (b"GIF87a", b"GIF89a"):
        return "image/gif"
    if head[:4] == b"RIFF" and head[8:12] == b"WEBP":
        return "image/webp"
    return None


def serve():
    session = Session()

    for raw in sys.stdin:
        raw = raw.strip()
        if not raw:
            continue
        try:
            msg = json.loads(raw)
        except Exception:
            log("ignoring non-JSON line from server")
            continue

        t = msg.get("t")
        mid = msg.get("id")

        try:
            if t == "hello":
                ok(mid,
                   adapter="claude/{}".format(ADAPTER_VERSION),
                   agent={"name": "claude", "version": claude_version()},
                   supports=SUPPORTS,
                   flags=FLAGS)

            elif t == "open":
                session.start(
                    workdir=msg["workdir"],
                    session_id=msg.get("sessionId"),
                    flags=msg.get("flags") or {},
                    permission_mode=msg.get("permissionMode"),
                )
                # The handshake that turns a process into a session: this is where
                # the project's real command list comes from, and where we declare
                # which dialogs we can render.
                #
                # `supportedDialogKinds` is first-attached-client-wins per the CLI's
                # own schema — "later initializes do not change it" — so this call is
                # the only one whose declaration counts. A per-turn re-initialize
                # cannot widen it later.
                initialized = session.control(
                    "initialize",
                    supportedDialogKinds=SUPPORTED_DIALOG_KINDS,
                )
                session.initialize_response = initialized
                flags = msg.get("flags") or {}
                session.current_model = flags.get("model")
                session.current_effort = flags.get("effort")
                surface = capabilities_from(
                    initialized, session.current_model, session.current_effort)
                ok(mid, sessionId=session.native_session_id, **surface)

            elif t == "send":
                # Config first, so the turn runs under the settings it was sent with.
                # Applied here rather than by the server as separate calls: one
                # message means there is no window in which a turn could begin under
                # a mix of old and new values.
                for config_id, value in (msg.get("config") or {}).items():
                    try:
                        session.apply_config(config_id, value)
                    except Exception as error:
                        # Not fatal. An agent refusing a setting it no longer offers
                        # is worth reporting, but losing the turn over it is worse
                        # than running under the previous value.
                        log("could not apply {}={}: {}".format(config_id, value, error))

                command = msg.get("command")
                text = msg.get("text") or ""
                if command:
                    # A prompt command is a kind of turn, not a side channel, so its
                    # output lands in the transcript like anything else. A control
                    # command never reaches here — it resolves against config.
                    args = " ".join(str(v) for v in (msg.get("args") or {}).values())
                    text = "/{} {}".format(command, args).strip()
                content = build_content(text, msg.get("attachments"))
                session.to_claude({"type": "user",
                                   "message": {"role": "user", "content": content}})
                ok(mid)

            elif t == "interrupt":
                session.control("interrupt")
                ok(mid)

            elif t == "steer":
                # The server has already checked the turn id against its own record;
                # rechecking here would need state this adapter does not keep, and
                # claude has no steer-with-precondition of its own. Carried in the
                # protocol so an agent that *can* enforce it does.
                session.to_claude({"type": "user",
                                   "message": {"role": "user",
                                               "content": msg.get("text") or ""}})
                ok(mid)

            elif t == "set_config":
                config_id = msg.get("configId")
                try:
                    session.apply_config(config_id, msg.get("value"))
                except UnknownConfig:
                    err(mid, "UNSUPPORTED_FIELD",
                        "'{}' is not settable on a claude session".format(config_id))
                    continue
                # The refreshed facet, not an acknowledgement: setting the model can
                # change which effort levels exist, so the caller needs the new
                # options rather than an assurance that the old ones still apply.
                ok(mid, facets={"config": config_from(
                    session.initialize_response,
                    session.current_model,
                    session.current_effort,
                )})

            elif t == "resolve":
                # `id` addresses the vendor request this answers, so it is passed
                # straight back through as claude's own request_id.
                session.answer(mid, msg.get("option"), msg.get("input"))

            elif t == "query":
                what = msg.get("what")
                if what == "context":
                    ok(mid, context=session.control("get_context_usage"))
                elif what == "cost":
                    ok(mid, cost=session.control("get_session_cost"))
                elif what == "plan":
                    ok(mid, plan=session.control("get_plan"))
                else:
                    err(mid, "UNSUPPORTED_QUERY", "cannot answer '{}'".format(what))

            elif t == "close":
                ok(mid)
                if session.proc and session.proc.poll() is None:
                    # Closing stdin is correct *here* and only here: the session
                    # is over, so there is no request left to strand.
                    try:
                        session.proc.stdin.close()
                    except Exception:
                        pass
                    try:
                        session.proc.wait(timeout=5)
                    except Exception:
                        session.proc.kill()
                return

            else:
                log("ignoring unknown message type '{}'".format(t))

        except Exception as error:
            if mid:
                err(mid, "ADAPTER_ERROR", str(error))
            else:
                log("error handling {}: {}".format(t, error))


def describe():
    """v1 compatibility. Kept so this directory still works with a server or a
    daemon that only speaks the one-shot contract."""
    print(json.dumps({
        "name": "claude",
        "description": "Anthropic Claude Code. Duplex session, live slash commands, approvals.",
        "session_key": "claude",
        "flags": FLAGS,
        "commands": {},
    }, indent=2))


if __name__ == "__main__":
    if "--serve" in sys.argv[1:]:
        serve()
    elif "--describe" in sys.argv[1:]:
        describe()
    else:
        log("usage: adapter.py --serve | --describe")
        sys.exit(2)
