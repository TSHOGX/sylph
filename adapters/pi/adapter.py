#!/usr/bin/env python3
"""pi adapter for `sylph.agent.v3`.

Translates between Sylph's normalized duplex protocol and `pi --mode rpc`, whose
own protocol is documented in the installed package at
`docs/rpc.md`. `docs/session-protocol.md` governs the Sylph side.

This adapter is why the protocol has a v3. Writing a second one is what turned
"claude does X" into "agents disagree about X", and three of those disagreements
had to be answered in the protocol rather than papered over here:

  * **pi has no tool-approval channel at all.** Its own security notes say it
    ships no sandbox, and there is no permission callback: a tool runs when the
    model calls it. So this adapter declares no `approval` capability, and a UI
    that reads `supports` will not offer an approval affordance for a pi session.
    That honesty is the whole point of negotiating capability up front.
  * **Its models and commands are queried, not pushed.** `get_available_models`,
    `get_commands` and `get_state` are three separate round trips, and the
    capability surface is assembled from them. Facet-scoped updates make that
    cheap: each answer speaks only about its own facet.
  * **Reasoning effort is per-model.** `get_available_thinking_levels` answers for
    the *currently selected* model — the one selected here offers only `off` and
    `high`, not the seven pi documents. A session-global effort enumeration would
    therefore be wrong, which is why config options are re-derived after a model
    change rather than cached.

pi also has a second queue discipline Sylph has no word for yet: a message can be
`steer` (delivered after the current tool calls, before the next model call) or
`followUp` (delivered only once the agent stops). Sylph's `steer` maps to the
former, which is the one that means "change course now".
"""
import json
import os
import subprocess
import sys
import threading
import uuid

PROTOCOL = "sylph.agent.v3"
ADAPTER_VERSION = "1.0.0"

# What this adapter can do, from the protocol's closed vocabulary.
#
# `approval` is absent deliberately and is not an omission to fix later: pi has no
# tool-permission callback, so declaring it would promise a prompt that can never
# arrive. `subagent` is absent for the same reason — pi reports no child
# trajectories.
SUPPORTS = [
    "interrupt",
    "steer",
    "slash",
    "set_config",
    "context",
    "fork",
]

# Spawn-time options. `model` lists nothing because the catalogue depends on which
# providers the user has configured — 21 on this machine — and is only knowable from
# a live session, where it becomes a config option instead.
FLAGS = {
    "model": {
        "choices": [],
        "description": "Model for this session, as `provider/id` or a pattern.",
    },
    "thinking": {
        "choices": ["off", "minimal", "low", "medium", "high", "xhigh", "max"],
        "description": "Thinking level. Which of these apply depends on the model.",
    },
}

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


class UnknownConfig(Exception):
    """A config id this adapter cannot set."""


class Session:
    """One `pi --mode rpc` process and the translation around it."""

    def __init__(self):
        self.proc = None
        self.native_session_id = None
        # Our request id -> a box the RPC reader fills. pi echoes the `id` we sent
        # on its `response`, so correlation is explicit rather than positional.
        self.pending = {}
        self.turn_open = False
        # contentIndex -> the in-flight block, since pi streams deltas by index and
        # never re-sends the cumulative text. Its docs are explicit that a client
        # needing a partial message must assemble it itself.
        self.blocks = {}
        self.message_seq = 0
        self.message_id = "pi"
        self.current_model = None
        self.current_thinking = None
        # vendor request id -> the option strings pi offered, so a picked index can
        # be resolved back to the literal value pi expects. It matches on what it
        # sent, not on an index it never saw.
        self.ui_options = {}
        self.lock = threading.Lock()

    # ---- vendor process -------------------------------------------------

    def start(self, workdir, session_id, flags, permission_mode):
        argv = ["pi", "--mode", "rpc"]

        # A resumed session is addressed by its own id. `--session-id` creates it if
        # missing, which is exactly the resume-or-start semantics `open` asks for.
        if session_id:
            argv += ["--session-id", session_id]
            self.native_session_id = session_id
        else:
            # Assigned here rather than discovered, so the server can correlate with
            # the archive before the first event lands.
            self.native_session_id = str(uuid.uuid4())
            argv += ["--session-id", self.native_session_id]

        if flags.get("model"):
            argv += ["--model", flags["model"]]
        if flags.get("thinking"):
            argv += ["--thinking", flags["thinking"]]

        soul = soul_path()
        if soul:
            # Additive, never replacing: Sylph is a voice layered over a working
            # coding agent, so the agent's own rules and the project's AGENTS.md
            # must survive. pi appends a file given this flag.
            argv += ["--append-system-prompt", soul]

        self.proc = subprocess.Popen(
            argv, cwd=workdir,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=None,
            text=True, encoding="utf-8", errors="replace", bufsize=1,
        )
        threading.Thread(target=self._read_vendor, daemon=True).start()

    def to_pi(self, obj):
        """Write to pi's stdin, which stays open for the session's life."""
        if not self.proc or self.proc.poll() is not None:
            raise RuntimeError("pi is not running")
        with self.lock:
            self.proc.stdin.write(json.dumps(obj, ensure_ascii=False) + "\n")
            self.proc.stdin.flush()

    def call(self, command_type, **fields):
        """Send an RPC command and wait for its response."""
        request_id = "sylph-{}".format(uuid.uuid4().hex[:12])
        box = {"event": threading.Event(), "response": None}
        self.pending[request_id] = box
        self.to_pi({"type": command_type, "id": request_id, **fields})
        if not box["event"].wait(timeout=30):
            self.pending.pop(request_id, None)
            raise RuntimeError("pi did not answer {} in time".format(command_type))
        reply = box["response"] or {}
        if not reply.get("success", True):
            raise RuntimeError(reply.get("error") or "pi refused {}".format(command_type))
        return reply.get("data") or {}

    # ---- capability surface ---------------------------------------------

    def commands(self):
        """pi's own command list, all of them prompts.

        pi already excludes its built-in TUI commands from `get_commands` and says
        so: they "would not execute if sent via prompt". So there is nothing to
        classify — everything it reports is genuinely something to ask the agent,
        and model and thinking are config options rather than commands.
        """
        commands = []
        for entry in (self.call("get_commands").get("commands") or []):
            if not isinstance(entry, dict) or not entry.get("name"):
                continue
            commands.append({
                "name": entry["name"],
                "description": entry.get("description") or "",
                # `extension`, `prompt`, or `skill`. Carried through as pi's own
                # word rather than remapped: it is advisory grouping, and inventing
                # a synonym would just lose information.
                "source": entry.get("source") or "prompt",
                "kind": "prompt",
            })
        return commands

    def config(self):
        """Model and thinking level, as typed options.

        Both are queried fresh. Thinking levels especially: they answer for the
        *currently selected* model, so a cached list goes stale the moment the model
        changes — which is the reason `set_config` returns the whole facet.
        """
        options = []

        models = self.call("get_available_models").get("models") or []
        if models:
            options.append({
                "type": "select",
                "id": "model",
                "name": "Model",
                "category": "model",
                "currentValue": self.current_model,
                "options": [
                    {
                        "value": model["id"],
                        "name": model.get("name") or model["id"],
                        # The provider is what disambiguates two models with the
                        # same display name, which happens across providers.
                        "description": model.get("provider"),
                    }
                    for model in models
                    if isinstance(model, dict) and model.get("id")
                ],
            })

        levels = self.call("get_available_thinking_levels").get("levels") or []
        if levels:
            options.append({
                "type": "select",
                "id": "thinking",
                "name": "Thinking",
                "category": "effort",
                "currentValue": self.current_thinking,
                "options": [{"value": level, "name": level.capitalize()} for level in levels],
            })

        return options

    def surface(self):
        """Every facet, for `open` and for a full refresh."""
        state = self.call("get_state")
        model = state.get("model")
        self.current_model = model.get("id") if isinstance(model, dict) else model
        self.current_thinking = state.get("thinkingLevel")

        surface = {
            "commands": self.commands(),
            "subagents": [],
            "config": self.config(),
        }
        # pi reports a per-model context window, so this facet is real here. The
        # claude adapter deliberately omits it: its `initialize` reports no window at
        # all, and inventing a number would put a fabricated denominator under a usage
        # meter — worse than showing none.
        if isinstance(model, dict) and model.get("contextWindow"):
            surface["contextWindow"] = model["contextWindow"]
        return surface

    def apply_config(self, config_id, value):
        if config_id == "model":
            self.call("set_model", model=value)
            self.current_model = value
        elif config_id == "thinking":
            self.call("set_thinking_level", level=value)
            self.current_thinking = value
        else:
            raise UnknownConfig(config_id)

    # ---- vendor -> sylph ------------------------------------------------

    def _read_vendor(self):
        for raw in self.proc.stdout:
            # Split on "\n" only, which `for raw in` does. pi's docs warn that a
            # generic line reader is not protocol-compliant here: U+2028 and U+2029
            # are legal inside JSON strings, and a reader that treats them as line
            # separators corrupts the record.
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
        event("status", "completed", text="pi exited", data={"exitCode": code})

    def _translate(self, msg):
        mtype = msg.get("type")

        if mtype == "response":
            box = self.pending.pop(msg.get("id"), None)
            if box:
                box["response"] = msg
                box["event"].set()
            return

        if mtype == "extension_ui_request":
            self._forward_ui_request(msg)
            return

        if mtype == "agent_start":
            if not self.turn_open:
                self.turn_open = True
                event("turn", "started")
            return

        if mtype == "message_start":
            self.message_seq += 1
            self.message_id = "pi-{}".format(self.message_seq)
            self.blocks = {}
            return

        if mtype == "message_update":
            self._delta(msg)
            return

        if mtype == "tool_execution_start":
            tool_id = msg.get("toolCallId") or "pi-tool"
            event("tool_call", "completed",
                  streamId=tool_id, name=msg.get("toolName"),
                  text=json.dumps(msg.get("args") or {}, ensure_ascii=False),
                  data={"input": msg.get("args")})
            return

        if mtype == "tool_execution_end":
            event("tool_result", "completed",
                  streamId=msg.get("toolCallId") or "pi-tool",
                  name=msg.get("toolName"),
                  text=flatten((msg.get("result") or {}).get("content")),
                  isError=bool(msg.get("isError")))
            return

        if mtype in ("compaction_start", "compaction_end"):
            if mtype == "compaction_end":
                event("compact", "completed", text="Context was compacted")
            return

        if mtype == "agent_settled":
            # `agent_settled`, not `agent_end`: pi is explicit that `agent_end` can
            # be followed by a retry, a compaction, or a queued continuation. Ending
            # the turn there would settle it while the agent was still working, and
            # the remaining output would arrive with no turn to belong to.
            self.turn_open = False
            event("turn", "completed", text="", data={"stopReason": "end_turn"})
            return

        if mtype == "extension_error":
            event("status", "completed",
                  text=str(msg.get("error") or "extension error"), isError=True)

    def _delta(self, msg):
        """One streaming delta, projected onto Sylph's event kinds."""
        delta = msg.get("assistantMessageEvent") or {}
        dtype = delta.get("type")
        index = delta.get("contentIndex", 0)

        kind = {
            "text": "assistant",
            "thinking": "thinking",
            "toolcall": "tool_call",
        }.get((dtype or "").rsplit("_", 1)[0])
        if not kind:
            return

        stream_id = "{}:{}".format(self.message_id, index)

        if dtype.endswith("_start"):
            self.blocks[index] = {"kind": kind, "id": stream_id, "text": ""}
            event(kind, "started", streamId=stream_id)
            return

        if dtype.endswith("_delta"):
            block = self.blocks.get(index)
            text = delta.get("delta") or ""
            if not text:
                return
            if block:
                block["text"] += text
            event(kind, "delta", streamId=stream_id, text=text)
            return

        if dtype.endswith("_end"):
            block = self.blocks.pop(index, None)
            # `content` on the end event is authoritative where pi sends it; the
            # accumulated deltas are the fallback.
            text = delta.get("content")
            if text is None:
                text = block["text"] if block else ""
            data = None
            if kind == "tool_call" and delta.get("toolCall"):
                data = {"input": (delta["toolCall"] or {}).get("arguments")}
            event(kind, "completed", streamId=stream_id, text=text, data=data)

    def _forward_ui_request(self, msg):
        """An extension asking the user something, projected onto a Sylph request.

        This is pi's only inversion — the agent asking a human — and it is not a
        permission channel: pi has no tool approvals. So these arrive as `choice`,
        `dialog` or `elicitation`, never as `approval`, which matches what this
        adapter declared it can raise.
        """
        method = msg.get("method")
        vendor_id = msg.get("id")

        # Fire-and-forget methods expect no reply. Answering one would send a
        # response pi is not waiting for; ignoring the display is allowed.
        if method in ("notify", "setStatus", "setWidget", "setTitle", "set_editor_text"):
            if method == "notify":
                event("notice", text=str(msg.get("message") or msg.get("title") or ""))
            return

        # pi resolves these itself when its own timeout lapses, so the deadline is
        # forwarded rather than dropped: a client that cannot show it is racing a
        # clock it cannot see.
        expires = None
        if isinstance(msg.get("timeout"), (int, float)):
            expires = int(now_ms() + msg["timeout"])

        if method == "select":
            offered = msg.get("options") or []
            self.ui_options[vendor_id] = offered
            options = [
                {"id": "option:{}".format(index), "label": str(label)}
                for index, label in enumerate(offered)
            ]
            options.append({"id": "cancel", "label": "Cancel"})
            emit({
                "t": "request", "id": vendor_id, "kind": "choice",
                "title": msg.get("title") or "pi has a question",
                "detail": {"options": offered},
                "options": options, "default": "cancel",
                "expiresAtMs": expires,
            })
            return

        if method == "confirm":
            emit({
                "t": "request", "id": vendor_id, "kind": "dialog",
                "title": msg.get("title") or "pi asks",
                "detail": {"message": msg.get("message")},
                "options": [
                    {"id": "allow", "label": "Yes"},
                    {"id": "deny", "label": "No"},
                ],
                "default": "deny",
                "expiresAtMs": expires,
            })
            return

        if method in ("input", "editor"):
            emit({
                "t": "request", "id": vendor_id, "kind": "elicitation",
                "title": msg.get("title") or "pi needs some text",
                "detail": {
                    "placeholder": msg.get("placeholder"),
                    "prefill": msg.get("prefill"),
                    "multiline": method == "editor",
                },
                "options": [
                    {"id": "submit", "label": "Submit"},
                    {"id": "cancel", "label": "Cancel"},
                ],
                "default": "cancel",
                "expiresAtMs": expires,
            })
            return

        # An unknown dialog method still blocks pi, so it is cancelled rather than
        # left parked. Silence here would stall the turn until pi's own timeout.
        log("unhandled extension UI method: {}".format(method))
        self.to_pi({"type": "extension_ui_response", "id": vendor_id, "cancelled": True})

    def answer(self, vendor_id, option, user_input):
        """Reply to an extension UI request in the shape its method expects.

        The three response shapes are not interchangeable — a `select` wants
        `value`, a `confirm` wants `confirmed`, and a cancel wants `cancelled` — and
        pi distinguishes "the user said no" from "the user dismissed it", because an
        extension can reasonably act differently on each.
        """
        option = str(option or "")

        if option == "cancel":
            self.ui_options.pop(vendor_id, None)
            self.to_pi({"type": "extension_ui_response", "id": vendor_id,
                        "cancelled": True})
            return

        if option == "deny":
            self.to_pi({"type": "extension_ui_response", "id": vendor_id,
                        "confirmed": False})
            return

        if option == "allow":
            self.to_pi({"type": "extension_ui_response", "id": vendor_id,
                        "confirmed": True})
            return

        if option.startswith("option:"):
            offered = self.ui_options.pop(vendor_id, [])
            try:
                index = int(option.split(":", 1)[1])
            except ValueError:
                index = -1
            if 0 <= index < len(offered):
                self.to_pi({"type": "extension_ui_response", "id": vendor_id,
                            "value": offered[index]})
            else:
                # An index that names nothing is a dismissal rather than a guess: a
                # wrong pick would hand the extension a choice the user never made.
                self.to_pi({"type": "extension_ui_response", "id": vendor_id,
                            "cancelled": True})
            return

        # `submit` on an input or editor: the text the user typed is the value.
        self.to_pi({"type": "extension_ui_response", "id": vendor_id,
                    "value": text_of(user_input)})


def text_of(user_input):
    """The text out of a resolve's `input`, whatever shape a client sent it in."""
    if user_input is None:
        return ""
    if isinstance(user_input, str):
        return user_input
    if isinstance(user_input, dict):
        for key in ("value", "text", "input"):
            if isinstance(user_input.get(key), str):
                return user_input[key]
    return json.dumps(user_input, ensure_ascii=False)


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


def now_ms():
    import time
    return int(time.time() * 1000)


def pi_version():
    try:
        result = subprocess.run(["pi", "--version"], capture_output=True,
                                text=True, timeout=10)
        return result.stdout.strip().split()[0] if result.returncode == 0 else None
    except Exception:
        return None


def build_message(text, attachments):
    """One user message, with images inline and other files named by path.

    pi takes images as `ImageContent` blocks and reads text files well with its own
    tools, so the split matches what the claude adapter does for the same reason:
    inlining a text file bloats the message with something the agent can fetch.
    """
    images, others = [], []
    for path in attachments or []:
        if not os.path.isfile(path):
            others.append(path)
            continue
        mime = image_mime(path)
        if mime:
            images.append((path, mime))
        else:
            others.append(path)

    body = prefix_paths(others, text)
    blocks = []
    if images:
        import base64
        for path, mime in images:
            try:
                with open(path, "rb") as handle:
                    data = base64.standard_b64encode(handle.read()).decode()
            except OSError as error:
                log("skipping unreadable image {}: {}".format(path, error))
                continue
            blocks.append({"type": "image", "data": data, "mimeType": mime})
    return body, blocks


def prefix_paths(paths, text):
    if not paths:
        return text
    lines = ["Files attached for this turn (absolute paths on local disk):"]
    lines += ["- " + str(path) for path in paths]
    lines += ["", text]
    return "\n".join(lines)


def image_mime(path):
    """Sniff the magic bytes; `mimetypes` misses enough real cases that the header
    is the more reliable signal, and a wrong guess sends a binary blob as text."""
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
                   adapter="pi/{}".format(ADAPTER_VERSION),
                   agent={"name": "pi", "version": pi_version()},
                   supports=SUPPORTS,
                   flags=FLAGS)

            elif t == "open":
                session.start(
                    workdir=msg["workdir"],
                    session_id=msg.get("sessionId"),
                    flags=msg.get("flags") or {},
                    permission_mode=msg.get("permissionMode"),
                )
                ok(mid, sessionId=session.native_session_id, **session.surface())

            elif t == "send":
                for config_id, value in (msg.get("config") or {}).items():
                    try:
                        session.apply_config(config_id, value)
                    except Exception as error:
                        # Not fatal: running the turn under the previous value beats
                        # losing the turn over a setting the agent no longer offers.
                        log("could not apply {}={}: {}".format(config_id, value, error))

                command = msg.get("command")
                text = msg.get("text") or ""
                if command:
                    args = " ".join(str(v) for v in (msg.get("args") or {}).values())
                    text = "/{} {}".format(command, args).strip()
                body, images = build_message(text, msg.get("attachments"))
                payload = {"type": "prompt", "message": body}
                if images:
                    payload["images"] = images
                session.to_pi(payload)
                ok(mid)

            elif t == "interrupt":
                session.call("abort")
                ok(mid)

            elif t == "steer":
                # pi's own `steer`, which delivers after the current tool calls and
                # before the next model call — the discipline that means "change
                # course now" rather than "queue this for later".
                session.to_pi({"type": "steer", "message": msg.get("text") or ""})
                ok(mid)

            elif t == "set_config":
                config_id = msg.get("configId")
                try:
                    session.apply_config(config_id, msg.get("value"))
                except UnknownConfig:
                    err(mid, "UNSUPPORTED_FIELD",
                        "'{}' is not settable on a pi session".format(config_id))
                    continue
                # The refreshed facet: setting the model changes which thinking
                # levels exist, so the old options may no longer be valid.
                ok(mid, facets={"config": session.config()})

            elif t == "resolve":
                session.answer(mid, msg.get("option"), msg.get("input"))

            elif t == "query":
                what = msg.get("what")
                if what in ("context", "cost"):
                    # One call answers both: pi reports tokens and cost together.
                    ok(mid, **{what: session.call("get_session_stats")})
                else:
                    err(mid, "UNSUPPORTED_QUERY", "cannot answer '{}'".format(what))

            elif t == "close":
                ok(mid)
                if session.proc and session.proc.poll() is None:
                    # Closing stdin is correct here and only here: the session is
                    # over, so there is no request left to strand.
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


if __name__ == "__main__":
    if "--serve" in sys.argv[1:]:
        serve()
    else:
        log("usage: adapter.py --serve")
        sys.exit(2)
