#!/usr/bin/env python3
"""Unit tests for the claude adapter's translation, with no CLI and no tokens.

`verify.py` is the end-to-end check and it cannot cover this: it spends real tokens,
needs a login, and only exercises whatever shapes one live turn happens to produce.
Every bug this file pins was a *classification* mistake on a payload the adapter had
already received — the kind that compiles, runs, and produces a plausible-looking
timeline that misdescribes what the agent did.

    python3 adapters/claude/adapter_test.py

Payload shapes here are copied from real `claude` 2.x output rather than invented. The
recurring lesson in this project's history is that guessing a vendor's field names or
values compiles and then silently does the wrong thing, so each case below records
what was measured.
"""
import json
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.argv = [sys.argv[0]]  # keep the adapter from reading this runner's arguments

import adapter  # noqa: E402


class Emitted(unittest.TestCase):
    """Captures what the adapter would have written to the server."""

    def setUp(self):
        self.events = []
        self._real_emit = adapter.emit
        adapter.emit = self.events.append
        self.session = adapter.Session()

    def tearDown(self):
        adapter.emit = self._real_emit

    def of_kind(self, kind):
        return [e for e in self.events if e.get("kind") == kind]

    def trajectories(self):
        return self.of_kind("subagent")


class BackgroundedBashIsNotADelegation(Emitted):
    """A `run_in_background` shell job reports through `task_*` too.

    Measured on claude 2.x: `task_started` carries `task_type: "local_bash"` for a
    backgrounded Bash and `"local_agent"` for a real delegation. Treating both as
    trajectories published a *shell command* as a subagent — a card named
    `find / -maxdepth 8 -ipath "*workflows*" …` — and its error flag escalated the
    Bash call hosting it, so a turn that had done nothing wrong read `failed` from its
    first moment. That is the exact reading a real session produced.
    """

    STARTED = {
        "type": "system", "subtype": "task_started",
        "task_id": "bg1", "tool_use_id": "toolu_bash",
        "description": 'find / -ipath "*workflows*"',
        "is_backgrounded": True, "spawn_depth": 1,
        "task_type": "local_bash",
    }

    def test_a_backgrounded_bash_opens_no_trajectory(self):
        self.session._task_notification("task_started", self.STARTED)
        self.assertEqual(self.trajectories(), [])

    def test_its_close_opens_nothing_either(self):
        # The trap this guards: neither `task_updated` nor `task_notification` carries
        # `task_type`, so a close filtered only at `started` would open the very node
        # its start was rejected from — and nothing would ever close *that*.
        self.session._task_notification("task_started", self.STARTED)
        self.session._task_notification("task_updated", {
            "type": "system", "subtype": "task_updated", "task_id": "bg1",
            "patch": {"status": "completed"},
        })
        self.session._task_notification("task_notification", {
            "type": "system", "subtype": "task_notification", "task_id": "bg1",
            "tool_use_id": "toolu_bash", "status": "completed", "summary": "BG_OK",
        })
        self.assertEqual(self.trajectories(), [])

    def test_a_real_delegation_still_opens_one(self):
        # The other half: filtering must not be so broad that it drops delegations.
        self.session._task_notification("task_started", {
            "type": "system", "subtype": "task_started",
            "task_id": "ag1", "tool_use_id": "toolu_agent",
            "description": "Read README.md first heading",
            "subagent_type": "Explore", "task_type": "local_agent",
        })
        opened = self.trajectories()
        self.assertEqual(len(opened), 1)
        self.assertEqual(opened[0]["phase"], "started")
        self.assertEqual(opened[0]["streamId"], "ag1")
        self.assertEqual(opened[0]["span"], "toolu_agent")

    def test_an_unknown_task_type_is_allowed_through(self):
        # A vendor adding a delegating kind should nest by default. Guessing wrong in
        # this direction costs a missing subtree; the other direction mislabels a tool
        # call as an agent, which is what actually shipped.
        self.session._task_notification("task_started", {
            "type": "system", "subtype": "task_started",
            "task_id": "new1", "tool_use_id": "toolu_new",
            "description": "something new", "task_type": "remote_agent",
        })
        self.assertEqual(len(self.trajectories()), 1)


class BackgroundedDelegationIsFlagged(Emitted):
    """`is_backgrounded` has to reach the server, on every phase.

    The server's turn-settle sweep closes trajectories an adapter left open, which is
    right when nothing more is coming — and wrong for backgrounded work, which is
    *expected* to outlive its turn and report its own outcome afterwards. Closing it as
    abandoned writes a contradiction its later completion cannot undo. Only
    `task_started` reports the flag, so the adapter has to remember and re-attach it or
    the sweep sees a close-worthy trajectory on every subsequent phase.
    """

    STARTED = {
        "type": "system", "subtype": "task_started",
        "task_id": "t1", "tool_use_id": "toolu_wf",
        "description": "long worker", "task_type": "local_agent",
        "is_backgrounded": True,
    }

    def test_the_flag_is_forwarded_on_the_opening_event(self):
        self.session._task_notification("task_started", self.STARTED)
        opened = [e for e in self.trajectories() if e["phase"] == "started"]
        self.assertTrue(opened[0]["data"]["is_backgrounded"])

    def test_it_is_re_attached_to_later_phases_that_omit_it(self):
        # The phase that matters: a progress notification carries no flag, and the
        # sweep reads whichever row it finds.
        self.session._task_notification("task_started", self.STARTED)
        self.session._task_notification("task_progress", {
            "type": "system", "subtype": "task_progress", "task_id": "t1",
            "tool_use_id": "toolu_wf", "description": "still going",
        })
        deltas = [e for e in self.trajectories() if e["phase"] == "delta"]
        self.assertTrue(deltas[-1]["data"]["is_backgrounded"])

    def test_a_foreground_delegation_is_not_flagged(self):
        # The exclusion must stay narrow, or an ordinary delegation left open becomes
        # the permanent spinner the sweep exists to prevent.
        self.session._task_notification("task_started", {
            "type": "system", "subtype": "task_started",
            "task_id": "t2", "tool_use_id": "toolu_wf2",
            "description": "quick worker", "task_type": "local_agent",
        })
        opened = [e for e in self.trajectories() if e["phase"] == "started"]
        self.assertNotIn("is_backgrounded", opened[0].get("data", {}))


class OneHostManyDelegates(Emitted):
    """`Workflow` fans out under a single `tool_use_id`.

    Keyed by the host, N delegates collapse into one row where the last name to arrive
    wins: a two-agent workflow rendered as one card reading `Demo: agent-2` and the
    other agent was absent from the timeline. The per-delegate identity is `task_id`.
    """

    def test_two_delegates_under_one_host_are_two_trajectories(self):
        for task, name in (("t1", "Demo: agent-1"), ("t2", "Demo: agent-2")):
            self.session._task_notification("task_started", {
                "type": "system", "subtype": "task_started",
                "task_id": task, "tool_use_id": "toolu_wf",
                "description": name, "task_type": "local_agent",
            })

        opened = [e for e in self.trajectories() if e["phase"] == "started"]
        self.assertEqual([e["streamId"] for e in opened], ["t1", "t2"])
        self.assertEqual({e["span"] for e in opened}, {"toolu_wf"})
        self.assertEqual([e["name"] for e in opened],
                         ["Demo: agent-1", "Demo: agent-2"])

    def test_a_close_with_no_tool_use_id_still_lands_in_the_tree(self):
        # `task_updated` carries no `tool_use_id` at all, so the host has to be
        # remembered from `task_started`. Without that the close lands at the root
        # while the trajectory it closes sits inside a container — leaving the
        # container open, which is the same spinner one level up.
        self.session._task_notification("task_started", {
            "type": "system", "subtype": "task_started",
            "task_id": "t1", "tool_use_id": "toolu_wf",
            "description": "worker", "task_type": "local_agent",
        })
        self.session._task_notification("task_updated", {
            "type": "system", "subtype": "task_updated", "task_id": "t1",
            "patch": {"status": "completed"},
        })

        closed = [e for e in self.trajectories() if e["phase"] == "completed"]
        self.assertEqual(len(closed), 1)
        self.assertEqual(closed[0]["streamId"], "t1")
        self.assertEqual(closed[0]["span"], "toolu_wf")


class ProgressIsASnapshot(Emitted):
    """These notifications re-send the whole current label, not a suffix.

    Appended as delta text it stacked six copies of one sentence in the journal, so it
    travels in `data` — which replaces — leaving `text` append-only as everywhere else
    in the protocol.
    """

    def test_repeated_progress_does_not_accumulate_text(self):
        self.session._task_notification("task_started", {
            "type": "system", "subtype": "task_started",
            "task_id": "t1", "tool_use_id": "toolu_wf",
            "description": "Working on it", "task_type": "local_agent",
        })
        for _ in range(3):
            self.session._task_notification("task_progress", {
                "type": "system", "subtype": "task_progress", "task_id": "t1",
                "tool_use_id": "toolu_wf", "description": "Working on it",
                "last_tool_name": "Read",
            })

        for event in self.trajectories():
            self.assertEqual(event.get("text", ""), "")
        deltas = [e for e in self.trajectories() if e["phase"] == "delta"]
        self.assertTrue(deltas)
        self.assertEqual(deltas[-1]["data"]["progress"], "Working on it")


class ClosingIsIdempotent(Emitted):
    """A close arrives two ways, and both may arrive.

    `task_updated` with a terminal `patch.status`, and `task_notification` with a
    terminal `status` plus the delegate's `summary`. Whichever lands first closes it;
    reopening a closed trajectory is what leaves a permanently running duplicate.
    """

    def setUp(self):
        super().setUp()
        self.session._task_notification("task_started", {
            "type": "system", "subtype": "task_started",
            "task_id": "t1", "tool_use_id": "toolu_wf",
            "description": "worker", "task_type": "local_agent",
        })

    def test_the_second_close_is_ignored(self):
        self.session._task_notification("task_updated", {
            "type": "system", "subtype": "task_updated", "task_id": "t1",
            "patch": {"status": "completed"},
        })
        self.session._task_notification("task_notification", {
            "type": "system", "subtype": "task_notification", "task_id": "t1",
            "tool_use_id": "toolu_wf", "status": "completed", "summary": "done",
        })
        closed = [e for e in self.trajectories() if e["phase"] == "completed"]
        self.assertEqual(len(closed), 1)

    def test_late_progress_does_not_reopen_it(self):
        self.session._task_notification("task_notification", {
            "type": "system", "subtype": "task_notification", "task_id": "t1",
            "tool_use_id": "toolu_wf", "status": "completed", "summary": "done",
        })
        before = len(self.events)
        self.session._task_notification("task_progress", {
            "type": "system", "subtype": "task_progress", "task_id": "t1",
            "tool_use_id": "toolu_wf", "description": "still going",
        })
        self.assertEqual(len(self.events), before)

    def test_a_failed_status_closes_with_an_error(self):
        self.session._task_notification("task_notification", {
            "type": "system", "subtype": "task_notification", "task_id": "t1",
            "tool_use_id": "toolu_wf", "status": "failed", "summary": "it broke",
        })
        closed = [e for e in self.trajectories() if e["phase"] == "completed"]
        self.assertEqual(len(closed), 1)
        self.assertTrue(closed[0]["isError"])
        self.assertEqual(closed[0]["text"], "it broke")


class TaskDelegationKeepsOneIdentity(Emitted):
    """The built-in `Task` declares its own trajectory, and must not get a second.

    claude forwards a `Task` delegate's events under the *spawning* id, so the block
    and the child's output share one identity. Minting a second id from `task_id`
    split one delegation into two nodes: the work landed on the declared id and the
    minted one stayed empty, never closing — which `verify.py` caught as a trajectory
    that was opened and then left with nothing in it.
    """

    def test_a_notification_for_a_declared_task_reuses_its_id(self):
        self.session.subagent_tools.add("toolu_task")
        self.session.subagent_parent["toolu_task"] = None
        self.session._task_notification("task_started", {
            "type": "system", "subtype": "task_started",
            "task_id": "inner", "tool_use_id": "toolu_task",
            "description": "Explore", "task_type": "local_agent",
        })

        opened = [e for e in self.trajectories() if e["phase"] == "started"]
        self.assertEqual([e["streamId"] for e in opened], ["toolu_task"])


class ForwardedOutputFindsItsDelegate(Emitted):
    """A host that announced one delegate is only its container.

    claude tags forwarded output with the *host*, so without re-tagging the work lands
    on the container while the delegate's own node stays empty — one delegation reading
    as two, one of them spinning. Only resolvable when the host has exactly one
    delegate; with several, claude forwards no interior at all, and guessing would put
    one delegate's work under another.
    """

    def test_output_is_attributed_to_a_lone_delegate(self):
        self.session._task_notification("task_started", {
            "type": "system", "subtype": "task_started",
            "task_id": "t1", "tool_use_id": "toolu_wf",
            "description": "worker", "task_type": "local_agent",
        })
        self.assertEqual(self.session._delegate_of("toolu_wf"), "t1")

    def test_a_multiplexing_host_keeps_its_own_id(self):
        for task in ("t1", "t2"):
            self.session._task_notification("task_started", {
                "type": "system", "subtype": "task_started",
                "task_id": task, "tool_use_id": "toolu_wf",
                "description": task, "task_type": "local_agent",
            })
        self.assertEqual(self.session._delegate_of("toolu_wf"), "toolu_wf")


class AttributionIsLatchedPerHost(Emitted):
    """`_delegate_of` reads a table that grows while the turn runs.

    So the same host has one answer before `task_started` and another after it, and the
    forwarded stream does not pause in between. Asked freshly each time, one delegate's
    output is split across the container and the delegate at whatever instant the
    notification happened to land — and *both halves render*, nested one inside the
    other, so no layer reports the split. That is the failure mode this whole family
    takes: a plausible timeline that misdescribes what the agent did.
    """

    def _announce(self, task="t1", host="toolu_wf"):
        self.session._task_notification("task_started", {
            "type": "system", "subtype": "task_started",
            "task_id": task, "tool_use_id": host,
            "description": "worker", "task_type": "local_agent",
        })

    def test_an_answer_given_before_the_delegate_arrived_stands(self):
        # Forwarded output reaching the adapter first can only be attributed to the
        # host. Once that is published, the delegate arriving later must not move the
        # rest of the same stream.
        self.assertEqual(self.session._delegate_of("toolu_wf"), "toolu_wf")
        self._announce()
        self.assertEqual(self.session._delegate_of("toolu_wf"), "toolu_wf")

    def test_a_delegate_known_first_is_used_for_the_whole_stream(self):
        self._announce()
        self.assertEqual(self.session._delegate_of("toolu_wf"), "t1")
        # A second delegate would make the host multiplexing, but the stream already
        # committed: re-deciding now would reparent output already published.
        self._announce(task="t2")
        self.assertEqual(self.session._delegate_of("toolu_wf"), "t1")

    def test_a_host_whose_sole_delegate_is_itself_resolves_to_itself(self):
        # A `Task` reports its notifications under the *spawning* id, so host and
        # trajectory are one. The identity case must not be recorded as a re-label.
        self.session.subagent_tools.add("toolu_task")
        self.session.subagent_parent["toolu_task"] = None
        self._announce(task="inner", host="toolu_task")
        self.assertEqual(self.session._delegate_of("toolu_task"), "toolu_task")

    def test_the_root_is_never_latched(self):
        self.assertIsNone(self.session._delegate_of(None))
        self.assertEqual(self.session.host_attribution, {})


class AForwardedBlockSurvivesANotification(Emitted):
    """A block's three phases must key on the envelope tag, not on the resolution.

    `parent_tool_use_id` is the only part claude guarantees is identical across
    `content_block_start`, `_delta` and `_stop`. Keyed by the trajectory that tag
    resolves to, a `task_started` landing mid-block moves the key out from under an open
    block: the delta looks up a tuple that no longer exists and returns, the stop misses
    too, and no `completed` is ever emitted. The block's whole text is dropped and its
    node stays open, with nothing at any layer reporting it.
    """

    def _forward(self, nested):
        self.session._stream_event(nested, "toolu_wf")

    def test_a_delta_arriving_after_a_task_started_still_lands(self):
        self._forward({"type": "content_block_start", "index": 0,
                       "content_block": {"type": "text", "text": ""}})
        # The notification that used to move the key.
        self.session._task_notification("task_started", {
            "type": "system", "subtype": "task_started",
            "task_id": "t1", "tool_use_id": "toolu_wf",
            "description": "worker", "task_type": "local_agent",
        })
        self._forward({"type": "content_block_delta", "index": 0,
                       "delta": {"text": "half a sentence"}})
        self._forward({"type": "content_block_stop", "index": 0})

        assistant = self.of_kind("assistant")
        self.assertEqual([e["phase"] for e in assistant],
                         ["started", "delta", "completed"])
        self.assertEqual(assistant[-1]["text"], "half a sentence")
        # And it stays in the trajectory its `started` claimed, rather than moving to
        # the delegate partway through one block.
        self.assertEqual({e.get("span") for e in assistant}, {"toolu_wf"})

    def test_concurrent_delegates_keep_their_own_index_zero(self):
        # The original keying bug, still pinned: two hosts both stream index 0.
        for host, text in (("toolu_a", "from A"), ("toolu_b", "from B")):
            self.session._stream_event(
                {"type": "content_block_start", "index": 0,
                 "content_block": {"type": "text", "text": ""}}, host)
        for host, text in (("toolu_a", "from A"), ("toolu_b", "from B")):
            self.session._stream_event(
                {"type": "content_block_delta", "index": 0,
                 "delta": {"text": text}}, host)
        for host in ("toolu_a", "toolu_b"):
            self.session._stream_event(
                {"type": "content_block_stop", "index": 0}, host)

        done = [e for e in self.of_kind("assistant") if e["phase"] == "completed"]
        self.assertEqual({(e["span"], e["text"]) for e in done},
                         {("toolu_a", "from A"), ("toolu_b", "from B")})


class PermissionModeIsValidated(unittest.TestCase):
    """A mode the CLI rejects must not reach it.

    Measured against claude 2.1.245 by trying each candidate: `--permission-mode`
    accepts `default`, `acceptEdits`, `auto`, `bypassPermissions`, `manual`, `dontAsk`
    and `plan`, and **rejects `ask`** — the value this protocol's own spec examples use
    for the field. commander rejects it at parse time, so the CLI exits 1 before
    answering `initialize` and the adapter reported "claude did not answer initialize in
    time": a timeout message for an argument error, which sends the investigation to the
    link layer while the cause is one argv entry.

    Note the vendor's own error text is not the authority either — it lists six values
    and omits `default`, which it nevertheless accepts.
    """

    def test_the_spec_s_own_example_value_is_not_accepted_by_the_cli(self):
        self.assertNotIn("ask", adapter.PERMISSION_MODES)

    def test_every_offered_spawn_choice_is_one_the_cli_accepts(self):
        # The flag schema and the CLI must not disagree: a mode offered at creation
        # that the CLI refuses is a session that cannot start, chosen from a picker.
        for choice in adapter.FLAGS["mode"]["choices"]:
            self.assertIn(choice, adapter.PERMISSION_MODES, choice)

    def test_every_interactive_mode_is_also_a_real_mode(self):
        for mode in adapter.INTERACTIVE_MODES:
            self.assertIn(mode, adapter.PERMISSION_MODES, mode)

    def test_manual_is_not_offered_because_the_vendor_normalizes_it(self):
        # Measured on 2.1.246: `set_permission_mode` answers **success** for `manual`
        # and confirms `default`. A row that lands on a different row is the same
        # defect as the bypass bug by another route, and only a read-back can see it —
        # an error check cannot, because there is no error.
        self.assertNotIn("manual", adapter.SETTABLE_PERMISSION_MODES)
        self.assertNotIn("manual", adapter.FLAGS["mode"]["choices"])

    def test_auto_is_offered_because_the_vendor_applies_it_as_given(self):
        # It was settable all along and absent from the offered list, so a real mode
        # was unreachable through the UI.
        self.assertIn("auto", adapter.SETTABLE_PERMISSION_MODES)

    def test_every_settable_mode_is_one_the_cli_knows(self):
        for mode in adapter.SETTABLE_PERMISSION_MODES:
            self.assertIn(mode, adapter.PERMISSION_MODES, mode)

    def test_the_live_picker_offers_exactly_the_settable_set(self):
        # The spawn flag's choices and the runtime setter's are different questions,
        # and answering the second with the first is what offered `manual` and hid
        # `auto`. Pinned on the produced surface rather than on the constant, so the
        # wiring is what is checked.
        options = adapter.config_from({"current_permission_mode": "default"})
        picker = next(o for o in options if o["id"] == "permissionMode")
        self.assertEqual(
            [choice["value"] for choice in picker["options"]],
            adapter.SETTABLE_PERMISSION_MODES,
        )


class AVendorRefusalIsNotASuccess(unittest.TestCase):
    """`control` must raise when claude answers `subtype: "error"`.

    This is the defect behind the reported bug. An error envelope carries `error` and
    **no** `response`, so the old `response.get("response") or {}` mapped a refusal onto
    the same `{}` a successful no-payload control returns. Selecting
    `bypassPermissions` was refused by the CLI ("because the session was not launched
    with --dangerously-skip-permissions"), recorded as applied, answered 200, and
    rendered as current — while every tool call went on asking for permission.

    Driven through `_translate` rather than by setting the box directly, because the
    unwrap that caused this lived in the dispatcher and a test that skipped it would
    pass against the bug.
    """

    def setUp(self):
        self.session = adapter.Session()
        self.sent = []
        self.session.to_claude = self.sent.append

    def _answer(self, envelope):
        """Feed one control_response for whatever request was just sent."""
        request_id = self.sent[-1]["request_id"]
        self.session._translate({
            "type": "control_response",
            "response": {"request_id": request_id, **envelope},
        })

    def _run(self, target, *args, **kwargs):
        """Call a blocking control in a thread so this test can answer it."""
        import threading
        box = {}

        def go():
            try:
                box["value"] = target(*args, **kwargs)
            except BaseException as error:      # noqa: BLE001 - re-raised below
                box["error"] = error

        thread = threading.Thread(target=go)
        thread.start()
        for _ in range(200):                    # wait for the request to be written
            if self.sent:
                break
            import time
            time.sleep(0.005)
        return thread, box

    def test_an_error_envelope_raises_rather_than_returning_empty(self):
        thread, box = self._run(self.session.control, "set_permission_mode",
                                mode="bypassPermissions")
        self._answer({
            "subtype": "error",
            "error": "Cannot set permission mode to bypassPermissions because the"
                     " session was not launched with --dangerously-skip-permissions",
        })
        thread.join(timeout=5)
        self.assertIsInstance(box.get("error"), adapter.VendorRefused)
        # The vendor's own sentence survives: it names the precondition, which is the
        # part a generic adapter error would destroy.
        self.assertIn("--dangerously-skip-permissions", str(box["error"]))

    def test_a_success_envelope_is_unwrapped_to_its_payload(self):
        thread, box = self._run(self.session.control, "set_permission_mode",
                                mode="plan")
        self._answer({"subtype": "success", "response": {"mode": "plan"}})
        thread.join(timeout=5)
        self.assertEqual(box.get("value"), {"mode": "plan"})

    def test_a_refused_permission_mode_does_not_become_the_reported_mode(self):
        self.session.initialize_response = {"current_permission_mode": "default"}
        thread, box = self._run(self.session.apply_config, "permissionMode",
                                "bypassPermissions")
        self._answer({"subtype": "error", "error": "refused"})
        thread.join(timeout=5)
        self.assertIsInstance(box.get("error"), adapter.VendorRefused)
        # The surface still reports what the session is actually running under. This is
        # the assertion the bug would fail: it used to read `bypassPermissions`.
        self.assertEqual(
            self.session.initialize_response["current_permission_mode"], "default")

    def test_the_confirmed_mode_is_recorded_rather_than_the_requested_one(self):
        # The `manual` case: success, but the vendor applied something else. Believing
        # the request here is how a picker reports a policy the session is not under.
        self.session.initialize_response = {"current_permission_mode": "default"}
        thread, _ = self._run(self.session.apply_config, "permissionMode", "manual")
        self._answer({"subtype": "success", "response": {"mode": "default"}})
        thread.join(timeout=5)
        self.assertEqual(
            self.session.initialize_response["current_permission_mode"], "default")


class TheApprovalChannelIsAlwaysRegistered(unittest.TestCase):
    """`--permission-prompt-tool stdio` may not be gated on the launch mode.

    The mode is mutable at runtime and the callback can only be asked for on the
    command line, so gating it bets the user will never switch. Measured on 2.1.246:
    launch in `bypassPermissions`, switch to `default`, and every tool call returns
    `Claude requested permissions to use Bash, but you haven't granted it yet` — no
    prompt can ever be raised, so such a session could not return to interactive use.

    Asserted on the argv the spawn would use, by intercepting `Popen`, because the
    ordering bug lived in argv construction and nothing below it could observe one.
    """

    def argv_for(self, mode):
        captured = {}

        class FakePopen:
            def __init__(self, argv, **kwargs):
                captured["argv"] = argv
                self.stdin = None
                self.stdout = []

            def poll(self):
                return None

        real_popen = adapter.subprocess.Popen
        real_thread = adapter.threading.Thread
        adapter.subprocess.Popen = FakePopen
        # The reader thread would iterate a list and exit, but starting it at all makes
        # the test depend on scheduling; stubbed for determinism.
        adapter.threading.Thread = lambda *a, **k: type(
            "NoThread", (), {"start": lambda self: None})()
        try:
            adapter.Session().start("/tmp", None, {}, mode)
        finally:
            adapter.subprocess.Popen = real_popen
            adapter.threading.Thread = real_thread
        return captured["argv"]

    def test_a_bypassing_launch_still_registers_the_channel(self):
        argv = self.argv_for("bypassPermissions")
        self.assertIn("--permission-prompt-tool", argv)
        self.assertEqual(argv[argv.index("--permission-prompt-tool") + 1], "stdio")

    def test_an_interactive_launch_registers_it_too(self):
        self.assertIn("--permission-prompt-tool", self.argv_for("default"))

    def test_bypass_is_made_reachable_without_starting_in_it(self):
        # Both halves matter: the flag has to be present, or `set_permission_mode` is
        # refused; and the launch mode has to stay as asked, or selecting bypass later
        # is indistinguishable from having started in it.
        argv = self.argv_for("default")
        self.assertIn("--allow-dangerously-skip-permissions", argv)
        self.assertNotIn("--dangerously-skip-permissions", argv)
        self.assertEqual(argv[argv.index("--permission-mode") + 1], "default")


class NestedTrajectoriesCarrySpanOnEveryPhase(Emitted):
    """A nested `subagent` must name its parent on *every* phase, not only on open.

    `history::consolidate` folds a declared trajectory's phases into one row keyed by
    its close, and it carries `name` forward but **not** `span`. So an adapter that
    states the parent only on `started` nests correctly while the turn is live and
    re-roots itself beside its parent the moment the turn settles — the delegated
    subtree flattens on reload, which is the pre-`span` bug arriving via the storage
    tier. Both of this adapter's two paths to a `subagent` event are pinned here
    because neither was covered: the existing specs assert a close lands in the tree,
    which passes even if an intermediate phase drops the pointer.
    """

    def test_a_nested_declared_task_names_its_parent_throughout(self):
        # The outer Task, at the root: `span` is absent rather than null, which is how
        # the protocol spells "the root turn".
        self.session._stream_event(
            {"type": "content_block_start", "index": 0,
             "content_block": {"type": "tool_use", "id": "toolu_outer",
                               "name": "Task"}}, None)
        # The inner one, declared inside it — the envelope tags it with the outer id.
        for nested in (
            {"type": "content_block_start", "index": 0,
             "content_block": {"type": "tool_use", "id": "toolu_inner",
                               "name": "Task"}},
            {"type": "content_block_delta", "index": 0,
             "delta": {"partial_json": '{"subagent_type":"Explore"}'}},
            {"type": "content_block_stop", "index": 0},
        ):
            self.session._stream_event(nested, "toolu_outer")
        # Its `tool_result` is the delegated work's outcome, so it closes it.
        self.session._tool_results({
            "parent_tool_use_id": "toolu_outer",
            "message": {"content": [{"type": "tool_result",
                                     "tool_use_id": "toolu_inner",
                                     "content": "done"}]},
        })

        inner = [e for e in self.trajectories() if e["streamId"] == "toolu_inner"]
        self.assertEqual([e["phase"] for e in inner],
                         ["started", "delta", "completed"])
        for phase in inner:
            self.assertEqual(phase.get("span"), "toolu_outer", phase["phase"])

        outer = [e for e in self.trajectories() if e["streamId"] == "toolu_outer"]
        self.assertNotIn("span", outer[0])

    def test_a_notified_delegation_names_its_host_throughout(self):
        opened = {
            "type": "system", "subtype": "task_started",
            "task_id": "t1", "tool_use_id": "toolu_wf",
            "description": "worker", "task_type": "local_agent",
        }
        self.session._task_notification("task_started", opened)
        self.session._task_notification("task_progress", {
            "type": "system", "subtype": "task_progress", "task_id": "t1",
            "tool_use_id": "toolu_wf", "description": "still going",
        })
        # The close carries no `tool_use_id` at all, so the host has to be remembered
        # — and it has to reach this phase, not merely be known internally.
        self.session._task_notification("task_updated", {
            "type": "system", "subtype": "task_updated", "task_id": "t1",
            "patch": {"status": "completed"},
        })

        phases = [e for e in self.trajectories() if e["streamId"] == "t1"]
        self.assertEqual([e["phase"] for e in phases],
                         ["started", "delta", "completed"])
        for phase in phases:
            self.assertEqual(phase.get("span"), "toolu_wf", phase["phase"])


class AnUnnamedTrajectoryStaysUnnamed(Emitted):
    """`name` is what the agent called this, so a guess must not be sent as one.

    `None` says nobody knows yet; a literal asserts the agent chose it. The assertion is
    journaled, and `history::consolidate` keeps the best name any phase gave a block
    (`event.name.or(names.remove(id))`) — so a later phase carrying the real brief
    normally overwrites a placeholder and the difference never shows. It shows exactly
    when the brief never finishes streaming: a delegation that fails early, or a link
    that dies mid-input, leaves the placeholder as the trajectory's durable name in
    `history.db`, indistinguishable from one an agent chose. A presentation default
    belongs at render time, where the client already has one.
    """

    def test_a_notified_delegation_with_no_description_sends_no_name(self):
        self.session._task_notification("task_started", {
            "type": "system", "subtype": "task_started",
            "task_id": "t1", "tool_use_id": "toolu_wf",
            "task_type": "local_agent",
        })
        opened = [e for e in self.trajectories() if e["phase"] == "started"]
        self.assertEqual(len(opened), 1)
        self.assertNotIn("name", opened[0])

    def test_a_declared_task_whose_brief_never_parses_sends_no_name(self):
        # The realistic failure: the input is streamed partial JSON, so a delegation
        # that dies mid-brief has nothing to be named from at any phase. Nothing here
        # may fill that in — least of all the vendor's tool name, which this field
        # exists to keep off the card.
        self.session._stream_event(
            {"type": "content_block_start", "index": 0,
             "content_block": {"type": "tool_use", "id": "toolu_x",
                               "name": "Task"}}, None)
        self.session._stream_event(
            {"type": "content_block_delta", "index": 0,
             "delta": {"partial_json": '{"subagent_ty'}}, None)
        self.session._stream_event({"type": "content_block_stop", "index": 0}, None)
        self.session._tool_results({
            "message": {"content": [{"type": "tool_result",
                                     "tool_use_id": "toolu_x",
                                     "content": "died", "is_error": True}]},
        })

        phases = [e for e in self.trajectories() if e["streamId"] == "toolu_x"]
        self.assertEqual([e["phase"] for e in phases],
                         ["started", "delta", "completed"])
        for phase in phases:
            self.assertNotIn("name", phase)
        self.assertTrue(phases[-1]["isError"])

    def test_a_real_name_is_still_reported_on_every_phase_that_has_one(self):
        # The subtraction must not cost the names that do arrive.
        self.session._stream_event(
            {"type": "content_block_start", "index": 0,
             "content_block": {"type": "tool_use", "id": "toolu_y",
                               "name": "Task"}}, None)
        self.session._stream_event(
            {"type": "content_block_delta", "index": 0,
             "delta": {"partial_json": '{"subagent_type":"Explore"}'}}, None)
        self.session._stream_event({"type": "content_block_stop", "index": 0}, None)
        self.session._tool_results({
            "message": {"content": [{"type": "tool_result",
                                     "tool_use_id": "toolu_y",
                                     "content": "found it"}]},
        })

        phases = [e for e in self.trajectories() if e["streamId"] == "toolu_y"]
        self.assertEqual([e.get("name") for e in phases],
                         [None, "Explore", "Explore"])


class UnaddressableNotification(Emitted):
    """A notification with no id at all still happened."""

    def test_it_becomes_a_notice_rather_than_being_dropped(self):
        self.session._task_notification("task_progress", {
            "type": "system", "subtype": "task_progress", "summary": "something",
        })
        self.assertEqual([e["kind"] for e in self.events], ["notice"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
