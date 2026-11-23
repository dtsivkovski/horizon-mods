import contextlib
import importlib.util
import io
import itertools
import json
import os
import pathlib
import re
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock

# Loaded by path: `python3 -I` leaves this folder off sys.path.
spec = importlib.util.spec_from_file_location("owner_update", pathlib.Path(__file__).with_name("owner_update.py"))
ou = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ou)

PATH = "mods/pack.toml"
TOP = 'id = "pack"\nkind = "resourcepack"\nname = "Pack"\nowners = ["alice"]\nowner_ids = [42]\n'
OLD = '\n[[versions]]\nversion = "1.0.0"\nfile = "pack-1.0.0.zip"\nsha256 = "' + "a" * 64 + '"\n'
NEW = '\n[[versions]]\nversion = "1.1.0"\nfile = "pack-1.1.0.zip"\nsha256 = "' + "b" * 64 + '"\n'
REVOKED = '[[revoked]]\nid = "other"\nreason = "r"\ndate = "2026-11-01"\n'


def run(base=TOP + OLD, head=TOP + NEW + OLD, main=None, revoked=REVOKED, author=42, changes=(("M", PATH),)):
    main = base if main is None else main
    return ou.eligible(list(changes), base.encode(), head.encode(), main.encode(), revoked.encode(), author)


class Eligible(unittest.TestCase):
    def test_an_owner_adding_versions_is_eligible(self):
        self.assertEqual(run(), ("pack", "resourcepack", ["1.1.0"]))
        # Newest first or last: the entries on main do both.
        self.assertEqual(run(head=TOP + OLD + NEW)[2], ["1.1.0"])
        self.assertEqual(run(head=TOP + NEW + NEW.replace("1.1.0", "1.2.0") + OLD)[2], ["1.1.0", "1.2.0"])

    def test_a_new_version_below_one_of_the_same_day_waits_for_a_reviewer(self):
        day = lambda v, d: v.replace("\nfile", f'\npublished = "{d}"\nfile')  # noqa: E731
        old, new = day(OLD, "2026-11-19"), day(NEW, "2026-11-19")
        self.assertEqual(run(base=TOP + old, head=TOP + new + old)[2], ["1.1.0"])
        self.assertEqual(run(base=TOP + old, head=TOP + old + day(NEW, "2026-11-20"))[2], ["1.1.0"])
        with self.assertRaisesRegex(ou.Refused, "below a version of the same day 2026-11-19"):
            run(base=TOP + old, head=TOP + old + new)
        for kind, ext in (("shader", ".hshader"), ("datapack", ".zip")):
            with self.subTest(kind=kind):
                top = TOP.replace("resourcepack", kind)
                old, new = OLD.replace(".zip", ext), NEW.replace(".zip", ext)
                self.assertEqual(run(base=top + old, head=top + new + old)[1], kind)

    def test_a_signed_entry_needs_signed_versions(self):
        top = TOP + 'key = "RWQ"\n'
        signed = NEW + 'signature = "untrusted comment: x"\n'
        self.assertEqual(run(base=top + OLD, head=top + signed + OLD)[0], "pack")
        with self.assertRaisesRegex(ou.Refused, "no signature"):
            run(base=top + OLD, head=top + NEW + OLD)

    def test_refused(self):
        cases = {
            "not in .* owner_ids": dict(author=7),
            "id True is not|not in": dict(author=True),
            "has no owner_ids": dict(base=TOP.replace("owner_ids = [42]\n", "") + OLD,
                                     head=TOP.replace("owner_ids = [42]\n", "") + NEW + OLD),
            "not in .*owner_ids.*": dict(base=TOP.replace("[42]", "[42.0]") + OLD,
                                         head=TOP.replace("[42]", "[42.0]") + NEW + OLD),
            "kind is 'native-mod'": dict(base=TOP.replace("resourcepack", "native-mod") + OLD,
                                         head=TOP.replace("resourcepack", "native-mod") + NEW + OLD),
            "kind is 'mod'": dict(base=TOP.replace("resourcepack", "mod") + OLD,
                                  head=TOP.replace("resourcepack", "mod") + NEW + OLD),
            "changes more than one": dict(changes=(("M", PATH), ("M", "README.md"))),
            "more than one modified": dict(changes=(("A", PATH),)),
            "more than one modified ": dict(changes=(("M", "archive/api-0.1/mods/pack.toml"),)),
            "changes more than its versions: kind": dict(head=TOP.replace("resourcepack", "shader") + NEW + OLD),
            "changes more than its versions: name": dict(head=TOP.replace('"Pack"', '"Pack 2"') + NEW + OLD),
            "changes more than its versions: owner_ids": dict(head=TOP.replace("[42]", "[42, 7]") + NEW + OLD),
            "changes more than its versions: key": dict(head=TOP + 'key = "RWQ"\n' + NEW + OLD),
            "only add versions": dict(head=TOP + NEW + OLD.replace("1.0.0.zip", "1.0.1.zip")),
            "only add versions ": dict(head=TOP + NEW),
            "only add versions  ": dict(head=TOP + OLD),
            "only add versions   ": dict(base=TOP + OLD + NEW.replace("1.1.0", "0.9.0"),
                                         head=TOP + NEW.replace("1.1.0", "0.9.0") + NEW + OLD),
            "changed on main": dict(main=TOP + NEW + OLD),
            "file is not a .zip": dict(head=TOP + NEW.replace("1.1.0.zip", "1.1.0.hmod") + OLD),
            "file is not a .hshader": dict(base=TOP.replace("resourcepack", "shader") + OLD,
                                           head=TOP.replace("resourcepack", "shader") + NEW + OLD),
            "in revoked.toml": dict(revoked=REVOKED + REVOKED.replace('"other"', '"pack"')),
            "doesn't parse": dict(head="id = "),
            "has id": dict(base=TOP.replace('"pack"', '"other"') + OLD, head=TOP.replace('"pack"', '"other"') + NEW),
        }
        for why, args in cases.items():
            with self.subTest(why=why), self.assertRaisesRegex(ou.Refused, why.strip()):
                run(**args)

    def test_an_archived_id_reactivation_waits_for_a_maintainer(self):
        # A new mods/<id>.toml for an id under archive/: its check's owner step stays red until a maintainer confirms
        # the owner by hand, so it never merges by itself, even from an owner adding only versions.
        for changes in ((("A", PATH),), (("A", PATH), ("M", "archive/native-v1/mods/pack.toml")),
                        (("A", PATH), ("D", "archive/api-0.1/mods/pack.toml"))):
            with self.subTest(changes=changes), self.assertRaisesRegex(ou.Refused, "more than one modified"):
                run(base="", main="", head=TOP + NEW + OLD, changes=changes)

    def test_same_is_strict_about_types(self):
        self.assertTrue(ou.same({"a": [1, "x"]}, {"a": [1, "x"]}))
        for a, b in ((1, 1.0), (1, True), ([1], (1,)), ({"a": 1}, {"a": 1, "b": 2})):
            with self.subTest(a=a, b=b):
                self.assertFalse(ou.same(a, b))


NTOP = TOP.replace("resourcepack", "native-mod")
NOLD = ('\n[[versions]]\nversion = "1.0.0"\npublished = "2026-11-10"\nfile = "pack.hmod"\nsha256 = "' + "a" * 64
        + '"\n')
NNEW = ('\n[[versions]]\nversion = "1.1.0"\npublished = "2026-11-20"\nfile = "pack.hmod"\nsha256 = "' + "b" * 64
        + '"\n')
OWNERS = "# vouched\n42 # alice: 5 reviewed\n\n7\n"
TRANSFERS = "# id old new login reason\nother 1 2 bob same person (#1)\n"


def native(base=NTOP + NOLD, head=NTOP + NNEW + NOLD, owners=OWNERS, transfers=TRANSFERS, mode="shadow", author=42,
           revoked=REVOKED):
    return ou.eligible([("M", PATH)], base.encode(), head.encode(), base.encode(), revoked.encode(), author,
                       owners.encode(), transfers.encode(), mode)


class Native(unittest.TestCase):
    def test_a_vouched_owner_adding_the_newest_version_is_eligible_in_shadow_and_on(self):
        for mode in ("shadow", "on"):
            self.assertEqual(native(mode=mode), ("pack", "native-mod", ["1.1.0"]))
        # Below the others in the file, as many entries list them, it is still the newest by day.
        self.assertEqual(native(head=NTOP + NOLD + NNEW)[2], ["1.1.0"])

    def test_the_kill_switch_is_off_unless_exactly_shadow_or_on(self):
        for value in ("", "off", "ON", "true", "1", "shadow ", "on\n"):
            with self.subTest(value=value):
                self.assertEqual(ou.mode(value), "off")
                with self.assertRaisesRegex(ou.Refused, "kind is 'native-mod' and NATIVE_AUTO_MERGE is off"):
                    native(mode=ou.mode(value))
        self.assertEqual((ou.mode("shadow"), ou.mode("on")), ("shadow", "on"))
        # The other kinds don't depend on it.
        self.assertEqual(run()[1], "resourcepack")

    def test_only_vouched_owners(self):
        with self.assertRaisesRegex(ou.Refused, r"id 7\) is not in mods/pack.toml's owner_ids"):
            native(author=7)
        with self.assertRaisesRegex(ou.Refused, r"id 42\) is not in .github/auto-merge-owners.txt"):
            native(owners="# nobody\n7\n")
        with self.assertRaisesRegex(ou.Refused, "is not in .github/auto-merge-owners.txt"):
            native(owners="")
        for bad in ("alice\n", "42 43\n", "0042\n", "-42\n", "42.0\n"):
            with self.subTest(bad=bad), self.assertRaisesRegex(ou.Refused, "line .* is not one numeric id"):
                native(owners=OWNERS + bad)
        self.assertEqual(ou.vouched(OWNERS.encode()), {42, 7})

    def test_transferred_and_revoked_ids_wait(self):
        with self.assertRaisesRegex(ou.Refused, "pack is in .github/owner-transfers.txt"):
            native(transfers=TRANSFERS + "pack 1 42 alice reviewed (#2)\n")
        # A comment that mentions the id is not a transfer.
        self.assertEqual(native(transfers=TRANSFERS + "# pack 1 42\n")[0], "pack")
        with self.assertRaisesRegex(ou.Refused, "pack is in revoked.toml"):
            native(revoked=REVOKED + REVOKED.replace('"other"', '"pack"'))
        one_file = REVOKED.replace('"other"', '"pack"\nsha256 = "' + "c" * 64 + '"')
        with self.assertRaisesRegex(ou.Refused, "pack is in revoked.toml"):
            native(revoked=REVOKED + one_file)

    def test_one_new_newest_hmod_and_nothing_else(self):
        newer = NNEW.replace("1.1.0", "1.2.0").replace("b" * 64, "c" * 64).replace("2026-11-20", "2026-11-21")
        inline = '{version = "1.1.0", published = "2026-11-20", file = "pack.hmod", sha256 = "' + "b" * 64 + '"}'
        cases = {
            "adds exactly one version": dict(head=NTOP + newer + NNEW + NOLD),
            "is not the newest": dict(head=NTOP + NNEW.replace("2026-11-20", "2026-11-01") + NOLD),
            "needs a `published` day": dict(head=NTOP + NNEW.replace('published = "2026-11-20"\n', "") + NOLD),
            "needs a `published` day ": dict(head=NTOP + NNEW.replace("2026-11-20", "Nov 20") + NOLD),
            "file is not a .hmod": dict(head=NTOP + NNEW.replace("pack.hmod", "pack.zip") + NOLD),
            "below a version of the same day": dict(head=NTOP + NOLD + NNEW.replace("2026-11-20", "2026-11-10")),
            "changes more than its versions: name": dict(head=NTOP.replace('"Pack"', '"P"') + NNEW + NOLD),
            "changes more than its versions: owner_ids": dict(head=NTOP.replace("[42]", "[42, 7]") + NNEW + NOLD),
            "changes more than its versions: kind": dict(head=TOP + NNEW + NOLD),
            "changes more than its versions: key": dict(head=NTOP + 'key = "RWQ"\n' + NNEW + NOLD),
            "needs an earlier version": dict(base=NTOP + "versions = []\n", head=NTOP + f"versions = [{inline}]\n"),
            "only add versions": dict(head=NTOP + NNEW + NOLD.replace("a" * 64, "d" * 64)),
        }
        for why, args in cases.items():
            with self.subTest(why=why), self.assertRaisesRegex(ou.Refused, why.strip()):
                native(**args)

    def test_the_packages_compared_are_the_newest_earlier_one_and_the_staged_new_one(self):
        older = NOLD.replace("1.0.0", "0.9.0").replace("2026-11-10", "2026-11-01").replace("a" * 64, "e" * 64)
        # Oldest first in the file: the newest earlier version is still 1.0.0.
        base = NTOP + older + NOLD
        pair = ou.native_pair("o/r", "pack", base.encode(), (base + NNEW).encode())
        release = "https://github.com/o/r/releases/download"
        self.assertEqual(pair, {
            "version": "1.1.0",
            "old_url": f"{release}/pack-1.0.0/pack.hmod", "old_sha256": "a" * 64,
            "new_url": f"{release}/staging/{'b' * 64}.hmod", "new_sha256": "b" * 64,
        })
        published = ou.native_pair("o/r", "pack", base.encode(), (base + NNEW).encode(), staged=False)
        self.assertEqual(published["new_url"], f"{release}/pack-1.1.0/pack.hmod")
        for bad in (NNEW.replace('"1.1.0"', '"../x"'), NNEW.replace('"pack.hmod"', '"a/b.hmod"'),
                    NNEW.replace('"pack.hmod"', '".hmod"'), NNEW.replace('version = "1.1.0"\n', ""),
                    NNEW.replace("b" * 64, "B" * 64)):
            with self.subTest(bad=bad), self.assertRaisesRegex(ou.Refused, "no release URL can hold"):
                ou.native_pair("o/r", "pack", base.encode(), (base + bad).encode())

    def test_pins_are_sdk_releases_whose_tag_is_their_commit(self):
        a, b, c, m = "a" * 40, "b" * 40, "c" * 40, "e" * 40
        tags = ["launcher@0.2.98", f"sdk-{a}", "nightly", f"sdk-{b}", f"sdk-{c}", "sdk-short", f"sdk-{a.upper()}"]
        commits = {f"sdk-{a}": a, f"sdk-{b}": b, f"sdk-{c}": "f" * 40}
        sources = {a: f'//! x\npub const REV: &str = "{m}";\n', b: None}
        pins = ou.sdk_pins(tags, commits.get, sources.get)
        self.assertEqual(pins, [(a, m), (b, None)])
        self.assertEqual(ou.pins_line(pins), f"{a}={m} {b}=-")
        many = [f"sdk-{i:040x}" for i in range(ou.MAX_SDK_RELEASES + 5)]
        self.assertEqual(len(ou.sdk_pins(many, lambda t: t[4:], lambda c: None)), ou.MAX_SDK_RELEASES)


class Workflow(unittest.TestCase):
    """The security contract of owner-update.yml, read as text: no YAML library on the runner's python3 -I."""
    TEXT = pathlib.Path(__file__).parent.parent.joinpath(".github/workflows/owner-update.yml").read_text()

    @classmethod
    def jobs(cls):
        body = cls.TEXT.split("\njobs:\n", 1)[1]
        parts = re.split(r"^  ([a-z-]+):\n", body, flags=re.M)
        return dict(zip(parts[1::2], parts[2::2]))

    @classmethod
    def runs(cls, text):
        """The text of every `run:` script in `text`."""
        out, lines = [], text.splitlines()
        for i, line in enumerate(lines):
            m = re.match(r"^(\s*)(- )?run: ?(.*)$", line)
            if not m:
                continue
            if m[3] not in ("|", ">-", ">"):
                out.append(m[3])
                continue
            indent = len(m[1]) + (2 if m[2] else 0)
            block = []
            for nxt in lines[i + 1:]:
                if nxt.strip() and len(nxt) - len(nxt.lstrip()) <= indent:
                    break
                block.append(nxt)
            out.append("\n".join(block))
        return out

    def test_the_jobs_and_their_permissions(self):
        jobs = self.jobs()
        self.assertEqual(list(jobs), ["decide", "inspect", "merge", "report", "replay", "replay-inspect"])
        self.assertIn("\npermissions: {}\n", self.TEXT)
        self.assertNotIn("pull_request_target", self.TEXT)

        def perms(job):
            block = re.search(r"^    permissions:\n((?:      .*\n)+)", jobs[job], re.M)
            return sorted(line.strip() for line in block[1].splitlines())

        self.assertEqual(perms("decide"), ["checks: read", "contents: read", "pull-requests: read"])
        self.assertEqual(perms("inspect"), ["contents: read"])
        self.assertEqual(perms("merge"), ["contents: read", "pull-requests: write"])
        self.assertEqual(perms("report"), ["contents: read", "issues: write", "pull-requests: write"])
        self.assertEqual(perms("replay"), ["contents: read", "pull-requests: read"])
        self.assertEqual(perms("replay-inspect"), ["contents: read"])

    def test_only_merge_holds_the_app_key_and_only_when_on(self):
        jobs = self.jobs()
        for name, text in jobs.items():
            if name != "merge":
                self.assertNotIn("secrets.", text, name)
                self.assertNotIn("registry-merge", text, name)
                self.assertNotIn("create-github-app-token", text, name)
        merge = jobs["merge"]
        self.assertIn("needs: [decide, inspect]", merge)
        self.assertIn("environment: registry-merge", merge)
        condition = re.search(r"if: >-\n((?:      .*\n)+)", merge)[1]
        for part in ("needs.decide.result == 'success'", "needs.decide.outputs.native != 'true'",
                     "vars.NATIVE_AUTO_MERGE == 'on'", "needs.inspect.result == 'success'",
                     "needs.inspect.outputs.verdict == 'pass'", "github.event_name == 'workflow_run'"):
            self.assertIn(part, condition)
        # The merge step checks again, authorizes again at one main and merges one at a time in the repository.
        self.assertIn('[ "$MODE" != on ] || [ "$VERDICT" != pass ]', merge)
        self.assertIn("run: python3 -I scripts/owner_update.py merge", merge)
        self.assertIn("    concurrency:\n      group: owner-update-merge\n      queue: max\n", merge)
        for name in ("BASE", "ENTRY", "REVOKED", "OWNERS", "TRANSFERS"):
            self.assertIn(f"      {name}: ${{{{ needs.decide.outputs.{name.lower()} }}}}\n", merge)
        self.assertIn("base: ${{ steps.decide.outputs.base }}", jobs["decide"])

    def test_report_runs_one_at_a_time_a_pull_request(self):
        report = self.jobs()["report"]
        self.assertIn("    concurrency:\n      group: owner-update-report-${{ needs.decide.outputs.pr }}\n"
                      "      queue: max\n", report)
        self.assertIn("run: python3 -I scripts/owner_update.py report", report)

    def test_every_checkout_is_main(self):
        # A dispatched replay would otherwise check out the branch it was started on.
        checkouts = self.TEXT.count("actions/checkout@")
        self.assertEqual(len(re.findall(r"(?m)^\s+ref:", self.TEXT)), checkouts)
        self.assertEqual(self.TEXT.count("          ref: main\n"), checkouts)
        self.assertEqual(self.TEXT.count("persist-credentials: false"), checkouts)

    def test_scripts_interpolate_no_expression(self):
        scripts = [(name, s) for name, text in self.jobs().items() for s in self.runs(text)]
        self.assertGreater(len(scripts), 8)
        for name, script in scripts:
            with self.subTest(job=name):
                self.assertNotIn("${{", script)

    def test_inspection_has_no_token(self):
        jobs = self.jobs()
        for name in ("inspect", "replay-inspect"):
            self.assertNotIn("GH_TOKEN", jobs[name])
            self.assertNotIn("github.token", jobs[name])
            self.assertIn("scripts/native-inspect.sh", jobs[name])
            # Built before any package is downloaded.
            self.assertLess(jobs[name].index("cargo build --locked"), jobs[name].index("native-inspect.sh"))
        inspect_script = pathlib.Path(__file__).with_name("native-inspect.sh").read_text()
        self.assertIn("env -i PATH=/usr/bin:/bin", inspect_script)
        self.assertNotIn("GH_TOKEN", inspect_script)
        self.assertNotRegex(inspect_script, r"\bgh ")

    def test_replay_and_report_never_merge(self):
        jobs = self.jobs()
        for name in ("replay", "replay-inspect"):
            self.assertNotRegex(jobs[name], r"merge|comment|labels|write")
        self.assertIn("if: github.event_name == 'workflow_dispatch'", jobs["replay"])
        for name in ("decide", "merge", "report"):
            self.assertIn("github.event_name == 'workflow_run'", jobs[name])
        self.assertNotIn("/merge", jobs["report"])


class PullRequest(unittest.TestCase):
    PULLS = [
        {"number": 1, "sha": "1" * 40, "repo": "someone/horizon-mods", "ref": "update"},
        {"number": 2, "sha": "2" * 40, "repo": "other/horizon-mods", "ref": "update"},
        {"number": 3, "sha": "3" * 40, "repo": None, "ref": "gone"},
    ]

    def test_found_by_head_repository_and_branch(self):
        self.assertEqual(ou.pick(self.PULLS, "1" * 40, "someone/horizon-mods", "update"), 1)
        for args in (("1" * 40, "other/horizon-mods", "update"), ("1" * 40, "someone/horizon-mods", "main"),
                     ("4" * 40, "someone/horizon-mods", "update")):
            with self.subTest(args=args), self.assertRaisesRegex(ou.Refused, "0 open pull requests"):
                ou.pick(self.PULLS, *args)
        with self.assertRaisesRegex(ou.Refused, "2 open pull requests"):
            ou.pick(self.PULLS + self.PULLS[:1], "1" * 40, "someone/horizon-mods", "update")

    def test_the_newest_check_run_counts(self):
        actions = {"slug": "github-actions"}
        ok = {"id": 1, "name": "check", "app": actions, "status": "completed", "conclusion": "success"}
        failed = {**ok, "id": 2, "conclusion": "failure"}
        self.assertTrue(ou.check_passed([ok]))
        self.assertFalse(ou.check_passed([ok, failed]))
        self.assertTrue(ou.check_passed([failed, {**ok, "id": 3}]))
        self.assertFalse(ou.check_passed([{**ok, "app": {"slug": "someone-else"}}]))
        self.assertFalse(ou.check_passed([{**ok, "name": "checks"}]))
        self.assertFalse(ou.check_passed([]))


class Main(unittest.TestCase):
    def test_a_refusal_sets_no_output_whatever_it_prints(self):
        # A file name can hold a newline: its line in the log must not be a workflow command that sets `pr`.
        def decide(env):
            raise ou.Refused("it changes more than one modified mods/<id>.toml: A x\n::set-output name=pr::1")

        with tempfile.TemporaryDirectory() as temp:
            output, summary = pathlib.Path(temp, "output"), pathlib.Path(temp, "summary")
            env = {"GITHUB_OUTPUT": str(output), "GITHUB_STEP_SUMMARY": str(summary)}
            with mock.patch.dict(ou.os.environ, env), mock.patch.object(ou, "decide", decide), \
                    contextlib.redirect_stdout(io.StringIO()) as out:
                self.assertEqual(ou.main(), 0)
            self.assertFalse(output.exists())
        first, *rest = out.getvalue().splitlines()
        self.assertRegex(first, r"^::stop-commands::[0-9a-f]{32}$")
        # The line is printed, after commands were stopped, and the token that would resume them never is.
        self.assertTrue(any(line.startswith("::set-output name=pr::1") for line in rest))
        self.assertNotIn(first.split("::")[2], "\n".join(rest))


class MainNative(unittest.TestCase):
    def test_a_native_decision_writes_its_packages_and_pins(self):
        out = {"pr": 5, "kind": "a native mod", "id": "pack", "entry": "1" * 40, "revoked": "2" * 40,
               "native": "true", "version": "1.1.0", "pins": "a" * 40 + "=-"}
        with tempfile.TemporaryDirectory() as temp:
            output, summary = pathlib.Path(temp, "output"), pathlib.Path(temp, "summary")
            env = {"GITHUB_OUTPUT": str(output), "GITHUB_STEP_SUMMARY": str(summary), "HEAD_SHA": "3" * 40}
            with mock.patch.dict(ou.os.environ, env), mock.patch.object(ou, "decide", lambda env: out), \
                    mock.patch.object(ou.sys, "argv", ["owner_update.py"]), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(ou.main(), 0)
            lines = output.read_text().splitlines()
            self.assertEqual(lines[0], "sha=" + "3" * 40)
            self.assertIn("native=true", lines)
            self.assertIn("pins=" + "a" * 40 + "=-", lines)
            self.assertIn("the package check decides", summary.read_text())


D = {"pr": "5", "sha": "3" * 40, "base": "4" * 40, "id": "pack", "native": "true", "entry": "1" * 40,
     "revoked": "2" * 40, "owners": "6" * 40, "transfers": "7" * 40}
BLOBS = {k: D[k] for k in ("entry", "revoked", "owners", "transfers")}
LIVE = {"state": "open", "merged": False, "draft": False, "base": {"ref": "main", "repo": {"full_name": "o/r"}},
        "head": {"sha": "3" * 40}}


class Merge(unittest.TestCase):
    def test_it_merges_when_main_still_has_what_the_decision_read(self):
        ou.authorize(D, "8" * 40, BLOBS, LIVE, "o/r")
        # A resource pack's decision rests on the entry and revoked.toml only.
        ou.authorize({**D, "native": "", "owners": "", "transfers": ""}, "8" * 40,
                     {"entry": D["entry"], "revoked": D["revoked"]}, LIVE, "o/r")

    def test_it_refuses_a_stale_decision(self):
        cases = {
            "mods/pack.toml on main": dict(blobs={**BLOBS, "entry": "9" * 40}),
            "revoked.toml on main": dict(blobs={**BLOBS, "revoked": "9" * 40}),
            ".github/auto-merge-owners.txt on main": dict(blobs={**BLOBS, "owners": "9" * 40}),
            ".github/owner-transfers.txt on main": dict(blobs={**BLOBS, "transfers": ""}),
            "no longer open": dict(pr={**LIVE, "state": "closed"}),
            "no longer open ": dict(pr={**LIVE, "merged": True}),
            "or is a draft": dict(pr={**LIVE, "draft": True}),
            "no longer goes into main": dict(pr={**LIVE, "base": {"ref": "release", "repo": {"full_name": "o/r"}}}),
            "no longer goes into main ": dict(pr={**LIVE, "base": {"ref": "main", "repo": {"full_name": "x/r"}}}),
            "head moved on": dict(pr={**LIVE, "head": {"sha": "9" * 40}}),
        }
        for why, args in cases.items():
            with self.subTest(why=why), self.assertRaisesRegex(ou.Refused, why.strip()):
                ou.authorize(D, "8" * 40, args.get("blobs", BLOBS), args.get("pr", LIVE), "o/r")

    def test_the_decision_is_read_back_by_its_shape(self):
        self.assertEqual(ou.decided({k.upper(): v for k, v in D.items()}), D)
        for key, bad in (("pr", "5\n"), ("sha", "3" * 39), ("base", ""), ("id", "../x"), ("native", "yes"),
                         ("entry", ""), ("owners", "-"), ("transfers", "x" * 40)):
            with self.subTest(key=key), self.assertRaisesRegex(ou.Refused, "malformed"):
                ou.decided({**{k.upper(): v for k, v in D.items()}, key.upper(): bad})

    def test_a_merge_onto_another_main_is_checked_there(self):
        main, changes, after = "8" * 40, [("M", "mods/pack.toml")], {"merged": True, "base": {"ref": "main"}}
        self.assertEqual(ou.landed(D, main, [main], {}, changes, after), [])
        # main moved in the window: fine while the authorizing files are the same there.
        self.assertEqual(ou.landed(D, main, ["9" * 40], BLOBS, changes, after), [])
        problems = ou.landed(D, main, ["9" * 40], {**BLOBS, "revoked": "0" * 40}, changes, after)
        self.assertEqual(problems, ["revoked.toml changed on main between the authorization and the merge"])
        self.assertRegex(ou.landed(D, main, [main], {}, changes + [("M", "README.md")], after)[0], "changes more")
        self.assertRegex(ou.landed(D, main, [main, "9" * 40], {}, changes, after)[0], "2 parents")
        self.assertRegex(ou.landed(D, main, [main], {}, changes, {**after, "base": {"ref": "x"}})[0],
                         "did not merge into main")

    def merged(self, live=LIVE, blobs=BLOBS, after_parent="8" * 40):
        calls = []

        def gh(*args, token=None):
            calls.append((args, token))
            if args[:3] == ("api", "-X", "PUT"):
                return json.dumps({"merged": True, "sha": "c" * 40})
            if args == ("api", "repos/o/r/pulls/5"):
                return json.dumps(live if len(calls) == 1 else {"merged": True, "base": {"ref": "main"}})
            raise AssertionError(args)

        def git(*args):
            if args[0] == "fetch":
                return b""
            if args[0] == "rev-parse":
                return after_parent.encode() + b"\n"
            if args[0] == "diff":
                return b"M\0mods/pack.toml\0"
            raise AssertionError(args)

        env = {"GH_REPO": "o/r", "MERGE_TOKEN": "app", "MODE": "on", "VERDICT": "pass",
               **{k.upper(): v for k, v in D.items()}}
        with mock.patch.object(ou, "gh", gh), mock.patch.object(ou, "git", git), \
                mock.patch.object(ou, "fetch_main", lambda: "8" * 40), \
                mock.patch.object(ou, "blob_id", lambda c, p: blobs[{v: k for k, v in ou.authorizing(
                    "pack", True).items()}[p]]):
            return ou.merge(env), calls

    def test_the_merge_call_is_the_apps_names_the_head_and_follows_the_authorization(self):
        (commit, problems), calls = self.merged()
        self.assertEqual((commit, problems), ("c" * 40, []))
        put = [(args, token) for args, token in calls if "PUT" in args]
        self.assertEqual(put, [(("api", "-X", "PUT", "repos/o/r/pulls/5/merge", "-f", "merge_method=squash", "-f",
                                 "sha=" + "3" * 40), "app")])
        self.assertEqual([token for _, token in calls if _ is not put[0][0]], [None, None])

    def test_no_merge_call_when_the_authorization_fails(self):
        for live, blobs in (({**LIVE, "draft": True}, BLOBS), (LIVE, {**BLOBS, "owners": "9" * 40})):
            with self.subTest(live=live, blobs=blobs):
                calls = []
                with mock.patch.object(ou, "gh", lambda *a, token=None: calls.append(a) or json.dumps(live)), \
                        mock.patch.object(ou, "fetch_main", lambda: "8" * 40), \
                        mock.patch.object(ou, "blob_id", lambda c, p: blobs[{v: k for k, v in ou.authorizing(
                            "pack", True).items()}[p]]), self.assertRaises(ou.Refused):
                    ou.merge({"GH_REPO": "o/r", "MERGE_TOKEN": "app", "MODE": "on", "VERDICT": "pass",
                              **{k.upper(): v for k, v in D.items()}})
                self.assertFalse(any("PUT" in a for a in calls))

    def test_a_native_merge_needs_the_switch_on_and_a_pass_itself(self):
        env = {"GH_REPO": "o/r", "MERGE_TOKEN": "app", **{k.upper(): v for k, v in D.items()}}
        for mode_, verdict in (("shadow", "pass"), ("on", "refused"), ("", "pass"), ("on", "")):
            with self.subTest(mode=mode_, verdict=verdict), mock.patch.object(ou, "gh", mock.Mock()) as gh, \
                    mock.patch.object(ou, "fetch_main", mock.Mock()) as fetch, \
                    self.assertRaisesRegex(ou.Refused, "only when NATIVE_AUTO_MERGE is on"):
                ou.merge({**env, "MODE": mode_, "VERDICT": verdict})
            self.assertFalse(gh.called or fetch.called)

    def test_a_merge_with_problems_fails_the_job_but_says_it_merged(self):
        with tempfile.TemporaryDirectory() as temp:
            output, summary = pathlib.Path(temp, "output"), pathlib.Path(temp, "summary")
            env = {"GITHUB_OUTPUT": str(output), "GITHUB_STEP_SUMMARY": str(summary), "PR": "5", "SHA": "3" * 40}
            with mock.patch.dict(ou.os.environ, env), mock.patch.object(ou.sys, "argv", ["owner_update.py", "merge"]), \
                    mock.patch.object(ou, "merge", lambda env: ("c" * 40, ["the merge changes more"])), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(ou.main(), 1)
            self.assertEqual(output.read_text(), "merged=true\n")
            self.assertIn("**the merge changes more**", summary.read_text())
            output.unlink()
            with mock.patch.dict(ou.os.environ, env), mock.patch.object(ou.sys, "argv", ["owner_update.py", "merge"]), \
                    mock.patch.object(ou, "merge", mock.Mock(side_effect=ou.Refused("x"))), \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(ou.main(), 0)
            self.assertFalse(output.exists())


class Report(unittest.TestCase):
    def plan(self, mode="shadow", verdict="pass", rules="", merged="", head="3" * 40):
        return ou.report_plan(mode, verdict, rules, merged, "3" * 40, head, "o/r")

    def test_the_comment_is_fixed_text(self):
        body, label = self.plan()
        self.assertEqual(body.splitlines()[0], ou.MARKER)
        self.assertIn("would have merged once auto-merge is on", body)
        self.assertFalse(label)
        body, label = self.plan(verdict="refused", rules="R15,R19")
        self.assertIn("refused rules R15, R19 ([what they mean]", body)
        self.assertTrue(label)
        for verdict, rules in (("refused", ""), ("refused", "R1;R2"), ("", ""), ("pass\n", "")):
            with self.subTest(verdict=verdict, rules=rules):
                self.assertIn("could not be checked automatically", self.plan(verdict=verdict, rules=rules)[0])
        self.assertIn("waits for a reviewer", self.plan(mode="on")[0])
        self.assertTrue(self.plan(mode="on")[1])

    def test_nothing_when_merged_off_or_the_head_moved_on(self):
        self.assertIsNone(self.plan(merged="true"))
        self.assertIsNone(self.plan(mode="off"))
        self.assertIsNone(self.plan(head="9" * 40))
        self.assertIsNone(self.plan(head=""))

    def report(self, heads):
        heads, writes = list(heads), []

        def gh(*args, token=None):
            if args[1].startswith("repos/o/r/pulls/"):
                return heads.pop(0) + "\n"
            if "--paginate" in args:
                return "11\n12\n"
            writes.append(args)
            return ""

        env = {"GH_REPO": "o/r", "PR": "5", "SHA": "3" * 40, "NATIVE_AUTO_MERGE": "on", "VERDICT": "refused",
               "RULES": "R15", "MERGED": ""}
        with mock.patch.object(ou, "gh", gh):
            return ou.report(env), writes

    def test_a_stale_head_writes_nothing(self):
        said, writes = self.report(["9" * 40])
        self.assertEqual((said, writes), ("nothing to report", []))
        # The head moves on while the comments are read: still nothing.
        said, writes = self.report(["3" * 40, "9" * 40])
        self.assertEqual(writes, [])
        self.assertIn("head moved on", said)
        # And before the label: the comment only.
        said, writes = self.report(["3" * 40, "3" * 40, "9" * 40])
        self.assertEqual([w[4] for w in writes], ["repos/o/r/issues/comments/12"])
        said, writes = self.report(["3" * 40] * 3)
        self.assertEqual(said, "commented and labelled")
        self.assertEqual(writes[-1], ("api", "--silent", "repos/o/r/issues/5/labels", "-f", "labels[]=needs-review"))


class Replay(unittest.TestCase):
    def test_only_open_pull_requests_into_main_and_commits_on_main(self):
        self.assertIsNone(ou.replayable(LIVE))
        self.assertEqual(ou.replayable({**LIVE, "state": "closed"}), "not open, or a draft")
        self.assertEqual(ou.replayable({**LIVE, "draft": True}), "not open, or a draft")
        self.assertEqual(ou.replayable({**LIVE, "base": {"ref": "dev"}}), "not into main")

    def test_commits_on_mains_first_parent_line_only(self):
        # m3 merged a side branch s0 - s1 with a merge commit: s1 is its second parent.
        parents = {"m3": "m2", "m2": "m1", "s1": "s0", "s0": "m1", "m1": ""}
        self.assertEqual(ou.first_parents(parents, "m3"), ["m3", "m2", "m1"])
        self.assertEqual(ou.first_parents({"a": "b", "b": "a"}, "a"), ["a", "b"])
        self.assertEqual(ou.first_parents({}, "m3"), [])

    def test_named_ones_that_are_not_are_refused_before_anything_is_fetched(self):
        fetched = []

        def gh(*args, token=None):
            if args[1] == "repos/o/r/pulls/5":
                return json.dumps({**LIVE, "draft": True})
            if args[1] == "repos/o/r/pulls/6":
                return json.dumps({**LIVE, "base": {"ref": "dev", "repo": {"full_name": "o/r"}}})
            if args[2] == f"repos/o/r/commits?sha={'8' * 40}&per_page=100":
                # c is main's second-parent side: 8's first parent is 9, a merge of 0 and c.
                return f"{'8' * 40} {'9' * 40}\n{'9' * 40} {'0' * 40}\n{'c' * 40} {'0' * 40}\n{'0' * 40} \n"
            raise AssertionError(args)

        env = {"GH_REPO": "o/r", "REPLAY_PRS": "5 6", "REPLAY_COMMITS": "c" * 40}
        with mock.patch.object(ou, "gh", gh), mock.patch.object(ou, "fetch_main", lambda: "8" * 40), \
                mock.patch.object(ou, "listed", lambda c, p: b""), mock.patch.object(ou, "blob", lambda c, p: b""), \
                mock.patch.object(ou, "git", lambda *a: fetched.append(a) or b""):
            rows, pairs = ou.replay(env)
        self.assertEqual(rows, [("#5", "refused: not open, or a draft"), ("#6", "refused: not into main"),
                                ("c" * 12, "refused: not a commit on main's first-parent history")])
        self.assertEqual((pairs, fetched), ([], []))


class FormsBot(unittest.TestCase):
    """check.yml's "Who submitted it": the marker counts only in a pull request opened by a REGISTRY_BOT login."""
    TEXT = pathlib.Path(__file__).parent.parent.joinpath(".github/workflows/check.yml").read_text()

    def submitter(self, login, bot, body="<!-- submitted-by: alice -->\nA mod."):
        step = self.TEXT.split("- name: Who submitted it\n", 1)[1]
        lines = step.split("run: |\n", 1)[1].splitlines()
        script = "\n".join(l[10:] for l in itertools.takewhile(lambda l: l.startswith(" " * 10) or not l, lines))
        with tempfile.TemporaryDirectory() as d:
            out = pathlib.Path(d, "out")
            # The runner's environment stays out: only what the step gets, and what starting bash needs.
            env = {k: os.environ[k] for k in ("PATH", "SYSTEMROOT") if k in os.environ}
            env.update(LOGIN=login, BODY=body, BOT=bot, GITHUB_OUTPUT=str(out))
            subprocess.run([shutil.which("bash"), "-e", "-c", script], env=env, check=True)
            return out.read_text().strip()

    def test_one_bot_as_before(self):
        self.assertEqual(self.submitter("ludovic111", "ludovic111"), "login=alice")
        self.assertEqual(self.submitter("mallory", "ludovic111"), "login=mallory")
        self.assertEqual(self.submitter("ludovic111", "ludovic111", body="no marker"), "login=ludovic111")
        # Exactly the listed login: no `[bot]` added or taken away.
        self.assertEqual(self.submitter("ludovic111[bot]", "ludovic111"), "login=ludovic111[bot]")

    def test_no_bot_trusts_no_marker(self):
        for bot in ("", " "):
            for login in ("ludovic111", "horizon-mods-submissions[bot]", "horizon-mods-bot[bot]"):
                self.assertEqual(self.submitter(login, bot), f"login={login}")

    def test_every_listed_bot_during_a_switch(self):
        bots = "ludovic111 horizon-mods-submissions[bot]"
        self.assertEqual(self.submitter("ludovic111", bots), "login=alice")
        self.assertEqual(self.submitter("horizon-mods-submissions[bot]", bots), "login=alice")
        # The App's name without `[bot]` is a plain account's, and brackets are text, not a pattern.
        for login in ("horizon-mods-submissions", "horizon-mods-submissionsb", "horizon-mods-submissions[", "mallory",
                      "ludovic111 horizon-mods-submissions[bot]", "[bot]"):
            self.assertEqual(self.submitter(login, bots), f"login={login}")
        self.assertEqual(self.submitter("horizon-mods-submissions[bot]", "horizon-mods-submissions[bot]"), "login=alice")
        self.assertEqual(self.submitter("horizon-mods-submissionsb", "horizon-mods-submissions[bot]"),
                         "login=horizon-mods-submissionsb")

if __name__ == "__main__":
    unittest.main()
