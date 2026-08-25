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


class UnaddressableNotification(Emitted):
    """A notification with no id at all still happened."""

    def test_it_becomes_a_notice_rather_than_being_dropped(self):
        self.session._task_notification("task_progress", {
            "type": "system", "subtype": "task_progress", "summary": "something",
        })
        self.assertEqual([e["kind"] for e in self.events], ["notice"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
