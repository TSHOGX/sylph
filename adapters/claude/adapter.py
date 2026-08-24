#!/usr/bin/env python3
"""Claude adapter for `sylph.agent.v2`.

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

PROTOCOL = "sylph.agent.v2"
ADAPTER_VERSION = "1.0.0"

# What this adapter can do, from the protocol's closed vocabulary. Declared
# rather than assumed: the server refuses to send anything absent from here, so
# an honest list is what keeps the UI from offering a control that does nothing.
SUPPORTS = [
    "interrupt",
    "steer",
    "approval",
    "subagent",
    "slash",
    "set_model",
    "context",
    "fork",
]

FLAGS = {
    "model": {
        "choices": [],  # Filled from the live `models` list at open time.
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


class Session:
    """One `claude` process and the translation around it."""

    def __init__(self):
        self.proc = None
        self.native_session_id = None
        self.pending_control = {}       # our control id -> threading.Event box
        self.tool_names = {}            # tool_use_id -> tool name, for results
        self.blocks = {}                # content block index -> in-flight block
        self.subagent_tools = set()     # tool_use_ids that are subagent spawns
        self.message_id = "claude"
        self.message_seq = 0
        self.turn_open = False
        # vendor request id -> 'can_use_tool' | 'dialog'. A permission and a dialog
        # take different response shapes, and guessing wrong parks the turn.
        self.request_kinds = {}
        # vendor request id -> the labels AskUserQuestion offered, so a picked
        # index can be resolved back to the label the CLI expects.
        self.dialog_options = {}
        # vendor request id -> the AskUserQuestion input, so a picked option can
        # be written back into the shape the tool expects.
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
            labels = self.dialog_options.pop(vendor_id, [])
            picked = None
            if str(option).startswith("option:"):
                index = int(str(option).split(":", 1)[1])
                if index < len(labels):
                    picked = labels[index]

            if picked is None:
                # Declining to answer is a denied tool call, which the agent
                # handles as "the user did not pick one" rather than as an error.
                self.to_claude({
                    "type": "control_response",
                    "response": {"subtype": "success", "request_id": vendor_id,
                                 "response": {"behavior": "deny",
                                              "message": "The user declined to answer."}},
                })
                return

            # Allowed, with the answer written into the tool input. The CLI reads
            # the choice back from `updatedInput`, so this is the channel by which
            # a picked option reaches the model.
            base = dict(self.question_inputs.pop(vendor_id, {}) or {})
            questions = base.get("questions") or []
            if questions:
                questions = [dict(q) for q in questions]
                questions[0]["selectedOptions"] = [picked]
                base["questions"] = questions
            self.to_claude({
                "type": "control_response",
                "response": {"subtype": "success", "request_id": vendor_id,
                             "response": {"behavior": "allow",
                                          "updatedInput": base or None}},
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
        if not allow and option != "submit" and not str(option).startswith("option:"):
            body = {"behavior": "cancelled"}
        elif str(option).startswith("option:"):
            # AskUserQuestion: the index the user picked, resolved back to the
            # option's own label — the CLI matches on what it offered, not on an
            # index it never sent us.
            index = int(str(option).split(":", 1)[1])
            offered = self.dialog_options.pop(vendor_id, [])
            chosen = offered[index] if index < len(offered) else None
            body = {"behavior": "completed",
                    "result": {"choice": chosen} if chosen is not None else {}}
        else:
            body = {"behavior": "completed"}
            if user_input is not None:
                body["result"] = user_input

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
            self._stream_event(msg.get("event") or {})
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

            options = [
                {"id": "allow", "label": "Allow once"},
                {"id": "allow_always", "label": "Always allow {}".format(tool or "this"),
                 "persistent": True},
                {"id": "deny", "label": "Deny"},
            ]
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
        """Turn an AskUserQuestion tool call into a choice the user can make.

        Only the first question is offered. The tool can carry several, but Sylph's
        request shape is one question with one set of options, and answering a
        multi-question call partially would hand Claude an incomplete reply. The
        remaining questions stay in `detail`, so a client can show what else was
        asked rather than silently dropping it.
        """
        payload = request.get("input") or {}
        questions = payload.get("questions") or []
        first = questions[0] if questions else {}

        labels = []
        options = []
        for index, option in enumerate(first.get("options") or []):
            if isinstance(option, dict):
                label = option.get("label") or ""
                description = option.get("description")
            else:
                label, description = str(option), None
            labels.append(label)
            options.append({
                "id": "option:{}".format(index),
                "label": label or "Option {}".format(index + 1),
                # Carried so a client can explain a choice rather than only name
                # it, which is most of what makes the terminal's picker readable.
                **({"description": description} if description else {}),
            })

        # Deny is still offered: the user must be able to decline to answer, and
        # the agent handles that as a refused tool call.
        options.append({"id": "deny", "label": "Don't answer"})

        self.dialog_options[vendor_id] = labels
        self.question_inputs[vendor_id] = payload
        # Answered through the permission path, because that is what it is on the
        # wire — the option index is translated back into the tool input.
        self.request_kinds[vendor_id] = "question"

        emit({
            "t": "request",
            "id": vendor_id,
            "kind": "choice",
            "tool": "AskUserQuestion",
            "toolUseId": request.get("tool_use_id"),
            "title": first.get("question") or "Claude has a question",
            "detail": {
                "header": first.get("header"),
                "multiSelect": bool(first.get("multiSelect")),
                "questions": questions,
            },
            "options": options,
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

        # AskUserQuestion: the agent offers concrete options, and forwarding them
        # verbatim is the whole point — this is the one dialog where the *agent*
        # decided what the choices are.
        if kind == "permission_ask_user_question":
            questions = payload.get("questions") or []
            first = questions[0] if questions else {}
            options = []
            labels = []
            for index, option in enumerate(first.get("options") or []):
                label = option.get("label") if isinstance(option, dict) else str(option)
                labels.append(label)
                options.append({"id": "option:{}".format(index), "label": label or "Option"})
            options.append({"id": "cancel", "label": "Cancel"})
            self.dialog_options[vendor_id] = labels
            emit({
                "t": "request",
                "id": vendor_id,
                "kind": "choice",
                "title": first.get("question") or "Claude has a question",
                "detail": {"header": first.get("header"), "questions": questions},
                "options": options,
                "default": "cancel",
            })
            return

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

    def _stream_event(self, nested):
        """Claude's partial-message stream, projected onto Sylph event kinds."""
        ntype = nested.get("type")

        if ntype == "message_start":
            self.message_seq += 1
            self.message_id = (nested.get("message") or {}).get("id") \
                or "claude-{}".format(self.message_seq)
            if not self.turn_open:
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
            self.blocks[index] = {
                "kind": kind, "id": stream_id, "name": name, "text": text,
            }
            if name:
                self.tool_names[stream_id] = name
                # A Task call is a subagent spawn, so its results are nested
                # rather than shown as one opaque blob of tool output.
                if name == "Task":
                    self.subagent_tools.add(stream_id)
            event(kind, "started", streamId=stream_id, name=name, text=text or None)
            return

        if ntype == "content_block_delta":
            index = nested.get("index", 0)
            block = self.blocks.get(index)
            if not block:
                return
            delta = nested.get("delta") or {}
            text = (delta.get("text") or delta.get("thinking")
                    or delta.get("partial_json") or "")
            if not text:
                return
            block["text"] += text
            event(block["kind"], "delta",
                  streamId=block["id"], name=block["name"], text=text)
            return

        if ntype == "content_block_stop":
            index = nested.get("index", 0)
            block = self.blocks.pop(index, None)
            if not block:
                return
            data = None
            if block["kind"] == "tool_call":
                # Tool input arrives as streamed partial JSON, so it is only
                # parseable once the block is closed.
                try:
                    data = {"input": json.loads(block["text"])}
                except Exception:
                    data = {"input": block["text"]}
            event(block["kind"], "completed",
                  streamId=block["id"], name=block["name"],
                  text=block["text"], data=data)

    def _tool_results(self, msg):
        message = msg.get("message") or {}
        content = message.get("content") or []
        if not isinstance(content, list):
            content = [content]

        # `--forward-subagent-text` tags a subagent's own output with the tool
        # call that spawned it. That tag is what lets the timeline nest it.
        parent = msg.get("parent_tool_use_id")

        for block in content:
            if not isinstance(block, dict):
                continue
            if block.get("type") != "tool_result":
                continue
            tool_id = block.get("tool_use_id") or "claude-tool-result"
            kind = "subagent" if tool_id in self.subagent_tools else "tool_result"
            event(kind, "completed",
                  streamId=tool_id,
                  name=self.tool_names.get(tool_id),
                  text=flatten(block.get("content")),
                  isError=bool(block.get("is_error")),
                  data={"parentToolUseId": parent} if parent else None)

    def _system(self, msg):
        subtype = msg.get("subtype")

        if subtype == "init":
            # `init` carries the permission mode claude actually settled on,
            # which is not always the one requested.
            event("capabilities", capabilities={
                "permissionMode": msg.get("permissionMode"),
            })
            return

        if subtype == "commands_changed":
            # A skill was edited or a plugin loaded, so the composer's list is
            # now stale. Re-read it rather than trusting the notification's
            # payload, which carries only that something changed.
            try:
                fresh = self.control("initialize")
                event("capabilities", capabilities=capabilities_from(fresh))
            except Exception as error:
                log("failed to refresh commands: {}".format(error))
            return

        if subtype == "compact_boundary":
            event("compact", "completed",
                  text="Context was compacted",
                  data=msg.get("compact_metadata"))
            return

        if subtype in ("task_started", "task_progress", "task_updated"):
            event("subagent", "started" if subtype == "task_started" else "delta",
                  streamId=msg.get("task_id") or msg.get("tool_use_id"),
                  name=msg.get("description") or msg.get("subagent_type"),
                  text=msg.get("summary") or msg.get("prompt"),
                  data={k: v for k, v in msg.items()
                        if k in ("task_id", "subagent_type", "tool_use_id")})
            return

        if subtype in ("error", "api_error"):
            event("status", "completed",
                  text=str(msg.get("error") or msg.get("message") or "claude error"),
                  isError=True)
            return

        if subtype == "notification":
            event("notice", text=str(msg.get("message") or ""))


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


def capabilities_from(initialize_response):
    """Project claude's `initialize` response onto Sylph's capability shape.

    This is the redesign's payoff in one function: `commands` here is the
    project's real list — skills, project commands, plugins — where a v1
    descriptor carried whatever its author typed by hand.
    """
    commands = []
    for entry in initialize_response.get("commands") or []:
        if isinstance(entry, dict):
            name = entry.get("name")
            if not name:
                continue
            commands.append({
                "name": name,
                "description": entry.get("description") or "",
                "argHint": entry.get("argNames") or entry.get("argumentHint"),
                "source": entry.get("source") or "skill",
            })
        elif isinstance(entry, str):
            commands.append({"name": entry, "description": ""})

    models = []
    for entry in initialize_response.get("models") or []:
        if isinstance(entry, dict) and entry.get("id"):
            models.append({"id": entry["id"], "label": entry.get("displayName") or entry["id"]})
        elif isinstance(entry, str):
            models.append({"id": entry, "label": entry})

    subagents = []
    for entry in initialize_response.get("agents") or []:
        if isinstance(entry, dict) and entry.get("name"):
            subagents.append({"name": entry["name"], "description": entry.get("description") or ""})
        elif isinstance(entry, str):
            subagents.append({"name": entry, "description": ""})

    return {
        "commands": commands,
        "models": models,
        "subagents": subagents,
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
                # The handshake that turns a process into a session: this is
                # where the project's real command list comes from, and where we
                # declare which dialogs we can render.
                surface = capabilities_from(
                    session.control(
                        "initialize",
                        supportedDialogKinds=SUPPORTED_DIALOG_KINDS,
                    )
                )
                ok(mid, sessionId=session.native_session_id, **surface)

            elif t == "send":
                command = msg.get("command")
                text = msg.get("text") or ""
                if command:
                    # A slash command is a kind of turn, not a side channel, so
                    # its output lands in the transcript like anything else.
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
                session.to_claude({"type": "user",
                                   "message": {"role": "user",
                                               "content": msg.get("text") or ""}})
                ok(mid)

            elif t == "set":
                if "model" in msg:
                    session.control("set_model", model=msg["model"])
                elif "permissionMode" in msg:
                    session.control("set_permission_mode", mode=msg["permissionMode"])
                elif "cwd" in msg:
                    session.control("set_cwd", cwd=msg["cwd"])
                else:
                    err(mid, "UNSUPPORTED_FIELD", "nothing settable in that message")
                    continue
                ok(mid)

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
