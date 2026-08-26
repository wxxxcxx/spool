# Window Lifecycle Probe

Diagnostic prototype for comparing Accessibility, public CGWindow state, and
private SkyLight notifications when an application withdraws a window without
terminating.

Run it while WeChat is open:

```sh
./examples/window_lifecycle_probe/run.sh
```

Then activate the WeChat main window and close that window without quitting
WeChat. The probe waits one second for delayed notifications and prints one of:

- `RESULT CONFIRMED_WITHDRAWN_WITHOUT_DESTROY_EVENT`: the target disappeared
  from the application's AX window list and is no longer on screen, but no AX or
  SkyLight destroy notification arrived.
- `RESULT DESTROY_EVENT_OBSERVED`: at least one usable destroy notification
  arrived for the target.
- `RESULT TARGET_RETURNED_WITH_SAME_ID`: the withdrawn target later returned
  with the same window ID.

To select an exact process or window:

```sh
./examples/window_lifecycle_probe/run.sh --pid 1234 --window-id 5678
```

This example uses private SkyLight notification APIs for diagnosis only. It
does not modify windows or Spool state. Stop it with `Ctrl-C`.
