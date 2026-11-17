# MPROC5 step 1 — evidence (unit level; no ART run yet)

## The item wire shapes came from the image, not from the source tree

Scanned with the repo's own scanner against the reference image
(`.cache/boot/android-34/dex/framework`):

```
SCAN android.app.servertransaction.PauseActivityItem
  calls=["writeBoolean", "writeBoolean", "writeInt", "writeBoolean", "writeBoolean"]
SCAN android.app.servertransaction.StopActivityItem
  calls=["writeInt"]
SCAN android.app.servertransaction.ResumeActivityItem
  calls=["writeInt", "writeBoolean", "writeBoolean", "writeBoolean"]
```

The two new `ParcelableSpec`s encode exactly that, and
`tests/parcelable.rs::codec_field_orders_match_the_framework_dex` re-verifies
them (`Verification::Full`, all calls consumed) every run — so a drift in a
future image fails that test instead of a live session. Note the difference from
`android-14.0.0_r75`: the source's `StopActivityItem` also carries
`super.writeToParcel`; the image writes only `mConfigChanges`, and the image wins.

## `cargo test -p acos-services -p acos-binder-peer`

Command (worktree env: `ACOS_FONT_DIR` set, `fixtures/build` hard-linked, no
`ACOS_HOME`, no `ACOS_BOOT_DIR`, no `XDG_RUNTIME_DIR`):

```
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
CARGO_TARGET_DIR=$WT/.cache/mproc5-target \
  cargo test --offline -p acos-services -p acos-binder-peer
```

`evidence-step1-tests.txt` is the full list: **54 `test result: ok` lines, 0
failures**, including

```
Running mproc5_top_of_task-…
test a_cross_process_resume_pauses_then_stops_the_previous_top ... ok
test finishing_the_new_top_resumes_the_activity_below_in_the_other_process ... ok
test a_same_process_resume_queues_nothing ... ok
test the_status_is_data_not_a_guess ... ok
test result: ok. 4 passed; 0 failed
Running parcelable-…
test codec_field_orders_match_the_framework_dex ... ok
test result: ok. 4 passed; 0 failed
```

What those rows assert, in the same terms the ART driver will:

- the cross-process resume queues **exactly two** calls, in order
  `PauseActivityItem` then `StopActivityItem`, both addressed at the losing
  process's tag and keyed on **A's own** activity token — read back out of the
  encoded `ClientTransaction` parcelable
  (`mActivityToken` binder + the named lifecycle request's class);
- `tops` moves to B and off A, and the published foreground tag (the launcher's
  `top_process`, i.e. whose probe sample is served) follows it;
- finishing B queues one `ResumeActivityItem` keyed on A's token when A is the
  activity below it and lives in the other process;
- a same-process resume queues nothing (no double delivery on top of the guest's
  own W38/ROT6 watchers);
- a bare state machine without a published tag records the truth and does not
  invent a delivery target.

## Not yet run (must be, before this step is called done)

The real-ART two-process driver run listed in `STATUS.md` §"Next action". Until
it is green, `proc-a-paused-or-stopped-under-b` / `proc-b-window-on-top` /
`back-from-b-returns-to-a` remain **unproven on ART**; they are already in the
driver's fatal set, so the driver cannot report a false pass.
