# Status  (written by qwen-ops monitor at 2026-10-17T21:19Z)

## Bot
```
OpenD :11111 DOWN | service pid - | now 21:22 UTC
[2026-10-17T03:24:21Z INFO  bot::service] bar 2026-10-16 16:00 ET | 96 symbols | sync 52s 
```
## Jobs
- com.qwenlab.brain: loaded, running pid 3106473, last exit 0
- com.qwenlab.improve: loaded, running pid 3106578, last exit 0
- com.qwenlab.dash: loaded, running pid 3106498, last exit 0
- com.qwenlab.trainer: loaded, running pid 3106510, last exit 0
- com.qwenlab.tick: loaded, idle, last exit 0
- com.qwenlab.monitor: loaded, running pid 3105866, last exit 0
- com.qwenlab.supervisor: loaded, idle, last exit 0
- com.qwenlab.resourcewatch: loaded, idle, last exit 0
- com.qwenlab.watchdog: loaded, idle, last exit 0
- com.claudemoomoobot.bot: loaded, running, last exit 1
## Learning loop
- results (cut A / alternate cut B): 284 4  | newest: 2026-10-17 19:52:14 UTC
- whiteboard edits accepted/refused (24 h): 6/0; active beliefs/plan items: 0 2
- winners remembered (>=15% over 14 sessions): 0 0 (count, best %) - see HIGHLIGHTS.md
- best results so far (interval|options @capital: mean % per 14-session block, training / holdout / noisy):
  5h|obv+top13 @5000 [C]: +3.95 / +3.83 / +4.06 / +2.56
  5h|feegate+top13 @5000 [A]: +3.25 / +5.18 / +1.45 / +2.30
  5h|rollstop+top13 @5000 [B]: +2.91 / +4.69 / +1.25 / +3.60
  5h|obv+top13 @5000 [A]: +2.85 / +2.74 / +2.96 / +1.36
  5h|gate:rs+top13 @5000 [A]: +2.73 / +5.68 / -0.03 / +2.50
## Monitor
- faults now: none
- fixed this run: lab updated to 07f627e, every job unloaded and loaded again restarted the learning loop: it ran older code than the installed binary bot redeployed at 07f627e research tools rebuilt at 07f627e
- needs the user: version drift from ops/versions.lock (expected: rustc 1.98.1); no automatic action OpenD is down or logged out: open the moomoo OpenD app and log in
- recent log:
    2026-10-17T20:44Z fixed: started the overdue daily fix report bot redeployed at ec8d63c research tools rebuilt at ec8d63c
    2026-10-17T20:44Z USER: version drift from ops/versions.lock (expected: rustc 1.98.1); no automatic action OpenD is down or logged out: open the moomoo OpenD app and log in
    2026-10-17T20:44Z ok
    2026-10-17T21:01Z fixed: bot redeployed at b5db4b4 research tools rebuilt at b5db4b4
    2026-10-17T21:01Z USER: version drift from ops/versions.lock (expected: rustc 1.98.1); no automatic action OpenD is down or logged out: open the moomoo OpenD app and log in
    2026-10-17T21:01Z ok
