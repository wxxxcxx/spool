import XCTest
import AppKit
import QuartzCore

@testable import SpoolBar

final class ModelsTests: XCTestCase {
  func testDefaultPreferencesShowWorkspaceNumbersAsLabels() {
    let preferences = BarPreferences()

    XCTAssertEqual(
      (1...9).map { preferences.workspaceLabel(for: UInt32($0)) },
      (1...9).map(String.init)
    )
  }

  func testWorkspaceCycleStopsAtBothEnds() {
    let numbers: [UInt32] = [1, 2, 4]

    XCTAssertEqual(WorkspaceCycle.target(numbers: numbers, current: 2, step: 1), 4)
    XCTAssertEqual(WorkspaceCycle.target(numbers: numbers, current: 2, step: -1), 1)
    XCTAssertNil(WorkspaceCycle.target(numbers: numbers, current: 1, step: -1))
    XCTAssertNil(WorkspaceCycle.target(numbers: numbers, current: 4, step: 1))
    XCTAssertNil(WorkspaceCycle.target(numbers: [1], current: 1, step: 1))
  }

  func testWorkspaceBarLayoutResolvesSectionsAndWindowOverflow() {
    let result = WorkspaceBarLayout.resolve(
      WorkspaceBarLayoutInput(
        bounds: CGRect(x: 0, y: 0, width: 150, height: 28),
        workspaceButtonWidth: 15,
        windowCount: 5,
        windowOffset: 0,
        metrics: WorkspaceBarMetrics(
          iconButtonSize: 22,
          navigationButtonWidth: 16,
          horizontalPadding: 7,
          workspaceSpacing: 5,
          windowSpacing: 3
        )
      )
    )

    XCTAssertEqual(result.previousWorkspaceFrame, CGRect(x: 7, y: 3, width: 16, height: 22))
    XCTAssertEqual(result.workspaceFrame, CGRect(x: 23, y: 3, width: 15, height: 22))
    XCTAssertEqual(result.nextWorkspaceFrame, CGRect(x: 38, y: 3, width: 16, height: 22))
    XCTAssertEqual(result.separatorFrame, CGRect(x: 56, y: 8, width: 1, height: 12))
    XCTAssertEqual(result.previousWindowFrame, CGRect(x: 59, y: 3, width: 16, height: 22))
    XCTAssertEqual(result.windowViewportFrame, CGRect(x: 75, y: 3, width: 52, height: 22))
    XCTAssertEqual(result.nextWindowFrame, CGRect(x: 127, y: 3, width: 16, height: 22))
    XCTAssertEqual(result.windowCapacity, 2)
    XCTAssertEqual(result.visibleWindowRange, 0..<2)
    XCTAssertFalse(result.canPageWindowsBackward)
    XCTAssertTrue(result.canPageWindowsForward)
  }

  func testWorkspaceBarLayoutClampsOverflowOffsetAtTheLastPage() {
    let result = WorkspaceBarLayout.resolve(
      WorkspaceBarLayoutInput(
        bounds: CGRect(x: 0, y: 0, width: 150, height: 28),
        workspaceButtonWidth: 15,
        windowCount: 5,
        windowOffset: 99,
        metrics: WorkspaceBarMetrics(
          iconButtonSize: 22,
          navigationButtonWidth: 16,
          horizontalPadding: 7,
          workspaceSpacing: 5,
          windowSpacing: 3
        )
      )
    )

    XCTAssertEqual(result.windowOffset, 3)
    XCTAssertEqual(result.visibleWindowRange, 3..<5)
    XCTAssertTrue(result.canPageWindowsBackward)
    XCTAssertFalse(result.canPageWindowsForward)
  }

  func testWorkspaceBarLayoutOmitsWindowSectionWhenWorkspaceIsEmpty() {
    let result = WorkspaceBarLayout.resolve(
      WorkspaceBarLayoutInput(
        bounds: CGRect(x: 0, y: 0, width: 150, height: 28),
        workspaceButtonWidth: 15,
        windowCount: 0,
        windowOffset: 0,
        metrics: WorkspaceBarMetrics(
          iconButtonSize: 22,
          navigationButtonWidth: 16,
          horizontalPadding: 7,
          workspaceSpacing: 5,
          windowSpacing: 3
        )
      )
    )

    XCTAssertNil(result.separatorFrame)
    XCTAssertEqual(result.windowViewportFrame, .zero)
    XCTAssertEqual(result.visibleWindowRange, 0..<0)
    XCTAssertFalse(result.showsWindowOverflow)
  }

  func testWorkspaceArrowButtonsSelectPreviousAndNextWorkspace() throws {
    let controller = RecordingSpoolController()
    let document = try state(workspaces: [
      (number: 1, windowIDs: [], selected: false),
      (number: 2, windowIDs: [], selected: true),
      (number: 3, windowIDs: [], selected: false),
    ])
    let view = WorkspaceBarView(
      displayID: 7,
      workspaces: document.spaces,
      preferences: BarPreferences(),
      client: controller,
      iconProvider: AppIconProvider()
    )
    let buttons = descendants(of: view, as: BarActionButton.self)
    let previous = buttons.first { $0.identifier == WorkspaceBarView.previousWorkspaceButtonID }
    let next = buttons.first { $0.identifier == WorkspaceBarView.nextWorkspaceButtonID }

    previous?.performClick(nil)
    next?.performClick(nil)

    XCTAssertNotNil(previous)
    XCTAssertNotNil(next)
    XCTAssertTrue(view.renderedPreviousWorkspaceNavigationVisible)
    XCTAssertTrue(view.renderedNextWorkspaceNavigationVisible)
    XCTAssertEqual(
      controller.selectedWorkspaces,
      [
        .init(displayID: 7, number: 1),
        .init(displayID: 7, number: 3),
      ]
    )

    let first = try state(workspaces: [
      (number: 1, windowIDs: [], selected: true),
      (number: 2, windowIDs: [], selected: false),
      (number: 3, windowIDs: [], selected: false),
    ])
    view.update(workspaces: first.spaces)
    XCTAssertFalse(view.renderedPreviousWorkspaceNavigationVisible)
    XCTAssertTrue(view.renderedNextWorkspaceNavigationVisible)

    let last = try state(workspaces: [
      (number: 1, windowIDs: [], selected: false),
      (number: 2, windowIDs: [], selected: false),
      (number: 3, windowIDs: [], selected: true),
    ])
    view.update(workspaces: last.spaces)
    XCTAssertTrue(view.renderedPreviousWorkspaceNavigationVisible)
    XCTAssertFalse(view.renderedNextWorkspaceNavigationVisible)
  }

  func testWorkspaceLabelDoesNotSwitchOnClick() throws {
    let controller = RecordingSpoolController()
    let document = try state(workspaces: [
      (number: 1, windowIDs: [], selected: false),
      (number: 2, windowIDs: [], selected: true),
      (number: 3, windowIDs: [], selected: false),
    ])
    let view = WorkspaceBarView(
      displayID: 7,
      workspaces: document.spaces,
      preferences: BarPreferences(),
      client: controller,
      iconProvider: AppIconProvider()
    )
    let workspace = descendants(of: view, as: BarActionButton.self)
      .first { $0.workspaceNumber == 2 }

    workspace?.performClick(nil)

    XCTAssertNotNil(workspace)
    XCTAssertTrue(controller.selectedWorkspaces.isEmpty)
  }

  func testWorkspaceScrollDirectionIsReversed() throws {
    let controller = RecordingSpoolController()
    let document = try state(workspaces: [
      (number: 1, windowIDs: [], selected: false),
      (number: 2, windowIDs: [], selected: true),
      (number: 3, windowIDs: [], selected: false),
    ])
    let view = WorkspaceBarView(
      displayID: 7,
      workspaces: document.spaces,
      preferences: BarPreferences(),
      client: controller,
      iconProvider: AppIconProvider()
    )
    let workspace = try XCTUnwrap(
      descendants(of: view, as: BarActionButton.self)
        .first { $0.workspaceNumber == 2 }
    )
    let cgEvent = try XCTUnwrap(
      CGEvent(
        scrollWheelEvent2Source: nil,
        units: .line,
        wheelCount: 1,
        wheel1: -1,
        wheel2: 0,
        wheel3: 0
      )
    )
    let event = try XCTUnwrap(NSEvent(cgEvent: cgEvent))

    workspace.scrollWheel(with: event)

    XCTAssertEqual(controller.selectedWorkspaces, [.init(displayID: 7, number: 1)])
  }

  func testWorkspaceAndDragInteractionsSendTargetedCommands() throws {
    let controller = RecordingSpoolController()
    let document = try state(workspaces: [
      (number: 1, windowIDs: [20, 10], selected: true),
      (number: 2, windowIDs: [30], selected: false),
    ])
    let view = WorkspaceBarView(
      displayID: 7,
      workspaces: document.spaces,
      preferences: BarPreferences(),
      client: controller,
      iconProvider: AppIconProvider()
    )

    XCTAssertEqual(view.cycleWorkspace(step: 1), 2)
    XCTAssertEqual(controller.selectedWorkspaces, [.init(displayID: 7, number: 2)])
    XCTAssertFalse(view.moveWindowToWorkspace(20))
    XCTAssertTrue(view.moveWindowToWorkspace(99))
    XCTAssertEqual(
      controller.moves,
      [.init(windowID: 99, displayID: 7, workspaceNumber: 1, follow: true)]
    )
  }

  func testBarRendersOnlyTheSelectedWorkspaceWindows() throws {
    let document = try state(workspaces: [
      (number: 1, windowIDs: [10], selected: false),
      (number: 2, windowIDs: [20, 30, 40, 50], selected: true),
      (number: 3, windowIDs: [], selected: false),
    ])
    let view = WorkspaceBarView(
      displayID: 1,
      workspaces: document.spaces,
      preferences: BarPreferences(animationStyle: .none),
      client: RecordingSpoolController(),
      iconProvider: AppIconProvider()
    )
    XCTAssertEqual(view.renderedWindowIDs, [20, 30, 40, 50])
    let workspaceButton = descendants(of: view, as: BarActionButton.self)
      .first { $0.workspaceNumber != nil }
    XCTAssertEqual(workspaceButton?.title, "2")
  }

  func testStateGroupsVisibleWorkspacesByDisplayAndNativeSpace() throws {
    let data = Data(
      #"""
      {
        "version": 2,
        "timestamp": 1,
        "active": {
          "display_id": 1,
          "native_workspace_id": 10,
          "virtual_workspace_number": 2,
          "focused_window_id": 42
        },
        "displays": [
          {"display_id": 1, "active": true, "native_workspace_id": 10, "virtual_workspace_number": 2},
          {"display_id": 2, "active": false, "native_workspace_id": 20, "virtual_workspace_number": 1}
        ],
        "virtual_workspaces": [
          {"number": 1, "native_workspace_id": 10, "display_id": 1, "selected": false, "active": false, "windows": []},
          {"number": 2, "native_workspace_id": 10, "display_id": 1, "selected": true, "active": true, "windows": []},
          {"number": 1, "native_workspace_id": 20, "display_id": 2, "selected": true, "active": false, "windows": []},
          {"number": 1, "native_workspace_id": 99, "display_id": 1, "selected": true, "active": false, "windows": []}
        ]
      }
      """#.utf8)

    let state = try JSONDecoder().decode(SpoolStateDocument.self, from: data)
    XCTAssertEqual(state.spaces(on: state.displays[0]).map(\.number), [1, 2])
    XCTAssertEqual(state.spaces(on: state.displays[1]).map(\.number), [1])
  }

  func testV3ActiveAndDisplaySpaceIDsDecode() throws {
    let data = Data(
      #"{"version":3,"timestamp":1,"active":{"display_id":1,"space_id":42},"capabilities":{"move_windows":false,"focus":false,"create":false,"delete":false},"displays":[{"display_id":1,"active":true,"visible_space_id":42}],"spaces":[]}"#.utf8)

    let state = try JSONDecoder().decode(SpoolStateDocument.self, from: data)
    XCTAssertEqual(state.active.spaceID, 42)
    XCTAssertEqual(state.displays.first?.visibleSpaceID, 42)
  }

  func testPlannerUsesGapBetweenNotchAndTray() {
    let geometry = notchedGeometry(
      trayFrames: [CGRect(x: 850, y: 670, width: 40, height: 30)]
    )
    let plan = SafeAreaPlanner.plan(
      contentWidth: 400,
      minimumWidth: 80,
      geometry: geometry,
      barHeight: 26
    )

    XCTAssertEqual(plan?.frame.minX, 574)
    XCTAssertEqual(plan?.frame.maxX, 846)
    XCTAssertEqual(plan?.frame.height, 26)
  }

  func testPlannerRightAlignsShortContentBesideTray() {
    let geometry = notchedGeometry(
      trayFrames: [CGRect(x: 850, y: 670, width: 40, height: 30)]
    )
    let plan = SafeAreaPlanner.plan(
      contentWidth: 120,
      minimumWidth: 80,
      geometry: geometry,
      barHeight: 26
    )

    XCTAssertEqual(plan?.frame.minX, 726)
    XCTAssertEqual(plan?.frame.maxX, 846)
  }

  func testPlannerFallsBackToRightHalfWithoutNotch() {
    let geometry = ScreenGeometry(
      frame: CGRect(x: 100, y: 0, width: 1000, height: 700),
      visibleFrame: CGRect(x: 100, y: 0, width: 1000, height: 670),
      safeTopInset: 0,
      auxiliaryLeft: nil,
      auxiliaryRight: nil,
      trayFrames: []
    )
    let plan = SafeAreaPlanner.plan(
      contentWidth: 200,
      minimumWidth: 80,
      geometry: geometry,
      barHeight: 26
    )

    XCTAssertEqual(plan?.frame.maxX, 1096)
    XCTAssertEqual(plan?.frame.minX, 896)
  }

  func testPlannerHidesBarWhenWorkspaceControlCannotFit() {
    let geometry = notchedGeometry(
      trayFrames: [CGRect(x: 650, y: 670, width: 350, height: 30)]
    )
    let plan = SafeAreaPlanner.plan(
      contentWidth: 300,
      minimumWidth: 90,
      geometry: geometry,
      barHeight: 26
    )

    XCTAssertNil(plan)
  }

  func testBarWindowGeometryKeepsMovedFrameInsideDisplay() {
    let frame = BarWindowGeometry.constrained(
      CGRect(x: 980, y: -20, width: 180, height: 40),
      to: CGRect(x: 0, y: 0, width: 1_000, height: 800),
      minimumSize: CGSize(width: 100, height: 28)
    )

    XCTAssertEqual(frame, CGRect(x: 820, y: 0, width: 180, height: 40))
  }

  func testBarWindowGeometryResizesFromBottomRight() {
    let frame = BarWindowGeometry.resized(
      CGRect(x: 700, y: 750, width: 200, height: 30),
      delta: CGPoint(x: 50, y: -20),
      within: CGRect(x: 0, y: 0, width: 1_000, height: 800),
      minimumSize: CGSize(width: 120, height: 28)
    )

    XCTAssertEqual(frame, CGRect(x: 700, y: 730, width: 250, height: 50))
  }

  func testBarWindowGeometryResizesFromTopLeft() {
    let frame = BarWindowGeometry.resized(
      CGRect(x: 700, y: 750, width: 200, height: 30),
      delta: CGPoint(x: 20, y: 10),
      edges: [.left, .top],
      within: CGRect(x: 0, y: 0, width: 1_000, height: 800),
      minimumSize: CGSize(width: 120, height: 28)
    )

    XCTAssertEqual(frame, CGRect(x: 720, y: 750, width: 180, height: 40))
  }

  func testContextMenuReflectsAndChangesBarLockState() throws {
    var locked = false
    let document = try state(windowIDs: [10])
    let view = WorkspaceBarView(
      displayID: 1,
      workspaces: document.spaces,
      preferences: BarPreferences(),
      client: SpoolClient(),
      iconProvider: AppIconProvider(),
      isBarLocked: { locked },
      onBarLockChanged: { locked = $0 }
    )
    guard let menu = view.menu, let lockItem = menu.items.first,
      let lockAction = lockItem.action
    else {
      return XCTFail("Expected bar context menu")
    }

    view.menuWillOpen(menu)
    XCTAssertEqual(lockItem.title, "Lock Bar")
    _ = NSApp.sendAction(lockAction, to: lockItem.target, from: lockItem)
    XCTAssertTrue(locked)
    view.menuWillOpen(menu)
    XCTAssertEqual(lockItem.title, "Unlock Bar")
    XCTAssertTrue(menu.items.contains { $0.title == "Settings" })
  }

  func testBarWindowStateRoundTripsThroughJSON() throws {
    let state = BarWindowState(
      frame: CGRect(x: 10, y: 20, width: 300, height: 40),
      isLocked: true
    )

    let decoded = try JSONDecoder().decode(
      BarWindowState.self,
      from: JSONEncoder().encode(state)
    )
    XCTAssertEqual(decoded, state)
  }

  func testAdjustableBarAppliesConfiguredSurfaceAppearance() throws {
    let preferences = BarPreferences(
      backgroundColorHex: "#112233CC",
      borderColorHex: "#AABBCCDD",
      borderWidth: 1.5,
      cornerRadius: 10
    )
    let document = try state(windowIDs: [10])
    let content = WorkspaceBarView(
      displayID: 1,
      workspaces: document.spaces,
      preferences: preferences,
      client: SpoolClient(),
      iconProvider: AppIconProvider()
    )
    let view = AdjustableWorkspaceBarView(
      content: content,
      preferences: preferences,
      minimumContentSize: CGSize(width: 80, height: 28),
      allowedFrame: CGRect(x: 0, y: 0, width: 1_000, height: 800),
      isLocked: false,
      onFrameChanged: { _ in }
    )

    XCTAssertEqual(
      view.renderedSurfaceTintColor,
      NSColor(spoolHex: preferences.backgroundColorHex)?.cgColor
    )
    XCTAssertEqual(view.renderedSurfaceMaterial, .menu)
    XCTAssertEqual(view.layer?.backgroundColor, NSColor.clear.cgColor)
    XCTAssertEqual(
      view.layer?.borderColor,
      NSColor(spoolHex: preferences.borderColorHex)?.cgColor
    )
    XCTAssertEqual(view.layer?.borderWidth, 1.5)
    XCTAssertEqual(view.layer?.cornerRadius, 10)
    XCTAssertTrue(view.layer?.masksToBounds == true)
  }

  func testAdjustableBarUpdatesCursorWhenPointerEntersResizeCorner() throws {
    let document = try state(windowIDs: [10])
    let content = WorkspaceBarView(
      displayID: 1,
      workspaces: document.spaces,
      preferences: BarPreferences(),
      client: SpoolClient(),
      iconProvider: AppIconProvider()
    )
    let view = AdjustableWorkspaceBarView(
      content: content,
      preferences: BarPreferences(),
      minimumContentSize: CGSize(width: 80, height: 28),
      allowedFrame: CGRect(x: 0, y: 0, width: 1_000, height: 800),
      isLocked: false,
      onFrameChanged: { _ in }
    )
    let window = NSWindow(
      contentRect: CGRect(x: 0, y: 0, width: 180, height: 32),
      styleMask: .borderless,
      backing: .buffered,
      defer: false
    )
    window.contentView = view
    guard let contentView = window.contentView else {
      return XCTFail("Expected window content view")
    }
    view.frame = contentView.bounds
    guard
      let event = NSEvent.mouseEvent(
        with: .mouseMoved,
        location: CGPoint(x: 1, y: 1),
        modifierFlags: [],
        timestamp: 0,
        windowNumber: window.windowNumber,
        context: nil,
        eventNumber: 0,
        clickCount: 0,
        pressure: 0
      )
    else {
      return XCTFail("Expected cursor update event")
    }
    NSCursor.arrow.set()
    defer { NSCursor.arrow.set() }

    view.mouseEntered(with: event)

    XCTAssertNotEqual(NSCursor.current, NSCursor.arrow)
  }

  func testMoveHandleUsesSixDotsAndOnlyShowsWhenUnlocked() throws {
    let document = try state(windowIDs: [10])
    let content = WorkspaceBarView(
      displayID: 1,
      workspaces: document.spaces,
      preferences: BarPreferences(),
      client: SpoolClient(),
      iconProvider: AppIconProvider()
    )
    let view = AdjustableWorkspaceBarView(
      content: content,
      preferences: BarPreferences(),
      minimumContentSize: CGSize(width: 80, height: 28),
      allowedFrame: CGRect(x: 0, y: 0, width: 1_000, height: 800),
      isLocked: false,
      onFrameChanged: { _ in }
    )

    XCTAssertEqual(view.renderedGripDotCount, 6)
    XCTAssertTrue(view.isMoveHandleVisible)
    view.setLocked(true)
    XCTAssertFalse(view.isMoveHandleVisible)
    view.setLocked(false)
    XCTAssertTrue(view.isMoveHandleVisible)
  }

  func testAdjustableBarPinsMovementToItsOwnerDisplay() throws {
    let document = try state(windowIDs: [10])
    let content = WorkspaceBarView(
      displayID: 1,
      workspaces: document.spaces,
      preferences: BarPreferences(),
      client: SpoolClient(),
      iconProvider: AppIconProvider()
    )
    let ownerFrame = CGRect(x: 1_000, y: 0, width: 800, height: 600)
    let view = AdjustableWorkspaceBarView(
      content: content,
      preferences: BarPreferences(),
      minimumContentSize: CGSize(width: 80, height: 28),
      allowedFrame: ownerFrame,
      isLocked: false,
      onFrameChanged: { _ in }
    )

    XCTAssertEqual(view.ownerDisplayFrame, ownerFrame)

    let updatedFrame = CGRect(x: -800, y: 0, width: 800, height: 600)
    view.updateAllowedFrame(updatedFrame)
    XCTAssertEqual(view.ownerDisplayFrame, updatedFrame)
  }

  func testTOMLConfigurationLoadsLabelsAndAppearance() throws {
    let preferences = try BarTOML.parse(
      #"""
      height = 36
      vertical_padding = 4
      position = "right"
      window_spacing = 6
      split_at_notch = false
      notch_workspace_layout = "active_left"
      selection_color = "#FF0000CC"
        active_workspace_color = "#00FF0044"
        background_color = "#202124E6"
        border_color = "#FFFFFFFF"
        border_width = 1.25
        corner_radius = 12
        show_shadow = false
        inactive_workspace_color = "#FF000022"
      floating_icon_style = "raised"
      show_workspace_label = false
      collapse_inactive_workspaces = true
      stack_rotation_step = 7
      stack_rotation_limit = 15
      show_focus_ring = false
      focus_ring_width = 2.5
      animation_style = "smooth"

      [workspace_labels]
      1 = "工作"
      2 = "聊天"
      """#)

    XCTAssertEqual(preferences.barHeight, 36)
    XCTAssertEqual(preferences.verticalPadding, 4)
    XCTAssertEqual(preferences.iconButtonSize, 28)
    XCTAssertEqual(preferences.iconSize, 24)
    XCTAssertEqual(preferences.windowSpacing, 6)
    XCTAssertEqual(preferences.selectionColorHex, "#FF0000CC")
    XCTAssertEqual(preferences.activeWorkspaceColorHex, "#00FF0044")
    XCTAssertEqual(preferences.backgroundColorHex, "#202124E6")
    XCTAssertEqual(preferences.borderColorHex, "#FFFFFFFF")
    XCTAssertEqual(preferences.borderWidth, 1.25)
    XCTAssertEqual(preferences.cornerRadius, 12)
    XCTAssertFalse(preferences.showsShadow)
    XCTAssertEqual(preferences.floatingIconStyle, .raised)
    XCTAssertFalse(preferences.showsFocusRing)
    XCTAssertEqual(preferences.focusRingWidth, 2.5)
    XCTAssertEqual(preferences.animationStyle, .smooth)
    XCTAssertEqual(preferences.workspaceLabel(for: 1), "工作")
    XCTAssertEqual(preferences.workspaceLabel(for: 3), "3")
  }

  func testTOMLTemplateMatchesDefaultPreferences() throws {
    XCTAssertEqual(try BarTOML.parse(BarTOML.template), BarPreferences())
  }

  func testExampleConfigurationMatchesDefaultPreferences() throws {
    let packageRoot = URL(fileURLWithPath: #filePath)
      .deletingLastPathComponent()
      .deletingLastPathComponent()
      .deletingLastPathComponent()
    let source = try String(
      contentsOf: packageRoot.appendingPathComponent("config.example.toml"),
      encoding: .utf8
    )
    XCTAssertEqual(try BarTOML.parse(source), BarPreferences())
  }

  func testConfigurationSavePreservesCommentsUnknownKeysAndLabels() throws {
    let directory = FileManager.default.temporaryDirectory
      .appendingPathComponent(UUID().uuidString, isDirectory: true)
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: directory) }
    let url = directory.appendingPathComponent("config.toml")
    try #"""
    height = 28 # keep this comment
    position = "right" # removed legacy option
    stack_rotation_step = 6 # removed legacy option
    custom_option = "preserve-me"

    [workspace_labels]
    1 = "work"
    """#.write(to: url, atomically: true, encoding: .utf8)

    let store = BarConfigurationStore(url: url)
    store.start()
    defer { store.stop() }
    var next = store.preferences
    next.windowSpacing = 8
    next.cornerRadius = 10
    try store.save(next)

    let updated = try String(contentsOf: url, encoding: .utf8)
    XCTAssertTrue(updated.contains("height = 28 # keep this comment"))
    XCTAssertFalse(updated.contains("position ="))
    XCTAssertTrue(updated.contains("custom_option = \"preserve-me\""))
    XCTAssertTrue(updated.contains("1 = \"work\""))
    XCTAssertTrue(updated.contains("window_spacing = 8"))
    XCTAssertTrue(updated.contains("corner_radius = 10"))
    XCTAssertTrue(updated.contains("background_color = \"#1C1C1ED9\""))
    XCTAssertFalse(updated.contains("stack_rotation_step"))
    XCTAssertEqual(try BarTOML.parse(updated), next)
  }

  func testConfigurationStoreMovesLegacyFileWithoutRewritingIt() throws {
    let directory = FileManager.default.temporaryDirectory
      .appendingPathComponent(UUID().uuidString, isDirectory: true)
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: directory) }

    let legacyURL = directory.appendingPathComponent("spool-bar/config.toml")
    let destinationURL = directory.appendingPathComponent("spool/bar.toml")
    try FileManager.default.createDirectory(
      at: legacyURL.deletingLastPathComponent(),
      withIntermediateDirectories: true
    )
    let source = """
      # Preserve this file byte-for-byte during migration.
      height = 31
      custom_option = "preserve-me"

      [workspace_labels]
      1 = "work"
      """
    try source.write(to: legacyURL, atomically: true, encoding: .utf8)

    let store = BarConfigurationStore(url: destinationURL, legacyURL: legacyURL)
    store.start()
    defer { store.stop() }

    XCTAssertFalse(FileManager.default.fileExists(atPath: legacyURL.path))
    XCTAssertEqual(try String(contentsOf: destinationURL, encoding: .utf8), source)
    XCTAssertEqual(store.preferences.barHeight, 31)
    XCTAssertEqual(store.preferences.workspaceLabels, [1: "work"])
  }

  func testConfigurationSaveUpdatesAndRemovesWorkspaceLabels() throws {
    let source = #"""
      height = 28

      [workspace_labels]
      # Keep label notes.
      1 = "work" # first
      """#
    var preferences = try BarTOML.parse(source)
    preferences.workspaceLabels = [1: "code", 2: "chat"]

    let updated = BarTOML.updating(source, with: preferences)
    XCTAssertTrue(updated.contains("# Keep label notes."))
    XCTAssertTrue(updated.contains("1 = \"code\" # first"))
    XCTAssertTrue(updated.contains("2 = \"chat\""))
    XCTAssertEqual(try BarTOML.parse(updated).workspaceLabels, [1: "code", 2: "chat"])

    preferences.workspaceLabels = [2: "chat"]
    let removed = BarTOML.updating(updated, with: preferences)
    XCTAssertFalse(removed.contains("1 = \"code\""))
    XCTAssertEqual(try BarTOML.parse(removed).workspaceLabels, [2: "chat"])
  }

  func testLegacyIconSizeDoesNotOverrideDerivedIconSize() throws {
    let preferences = try BarTOML.parse(
      #"""
      height = 34
      vertical_padding = 4
      icon_size = 30
      """#)

    XCTAssertEqual(preferences.iconButtonSize, 26)
    XCTAssertEqual(preferences.iconSize, 22)
  }

  func testWorkspaceNumberRemainsVisibleWithoutCustomLabel() throws {
    let document = try state(windowIDs: [], selected: true)
    let view = WorkspaceBarView(
      displayID: 1,
      workspaces: document.spaces,
      preferences: BarPreferences(),
      client: SpoolClient(),
      iconProvider: AppIconProvider()
    )
    let workspaceButton = descendants(of: view, as: BarActionButton.self)
      .first { $0.workspaceNumber == 1 }

    XCTAssertEqual(workspaceButton?.title, "1")
    XCTAssertNil(workspaceButton?.image)
    XCTAssertFalse(workspaceButton?.isHidden == true)
  }

  func testWorkspaceControlUsesActiveColor() throws {
    let active = try state(windowIDs: [10], selected: true)
    let preferences = BarPreferences(
      activeWorkspaceColorHex: "#00FF0080",
      animationStyle: .none
    )
    let view = WorkspaceBarView(
      displayID: 1,
      workspaces: active.spaces,
      preferences: preferences,
      client: SpoolClient(),
      iconProvider: AppIconProvider()
    )
    let workspaceButton = descendants(of: view, as: BarActionButton.self)
      .first { $0.workspaceNumber == 1 }
    XCTAssertEqual(
      workspaceButton?.renderedSelectionFillColor,
      NSColor(spoolHex: "#00FF0080")?.cgColor
    )
  }

  func testWindowButtonsFollowSpoolWindowOrder() throws {
    let first = try state(windowIDs: [20, 10])
    let view = WorkspaceBarView(
      displayID: 1,
      workspaces: first.spaces,
      preferences: BarPreferences(
        barHeight: 28,
        itemSpacing: 5,
        horizontalPadding: 7
      ),
      client: SpoolClient(),
      iconProvider: AppIconProvider()
    )
    XCTAssertEqual(view.renderedWindowIDs, [20, 10])

    let reordered = try state(windowIDs: [10, 20])
    view.update(workspaces: reordered.spaces)
    XCTAssertEqual(view.renderedWindowIDs, [10, 20])
  }

  func testFloatingWindowButtonsAreAlwaysPlacedLast() throws {
    let document = try state(
      windowIDs: [30, 20, 10, 40],
      floatingWindowIDs: Set([20, 40])
    )
    let view = WorkspaceBarView(
      displayID: 1,
      workspaces: document.spaces,
      preferences: BarPreferences(
        barHeight: 28,
        itemSpacing: 5,
        horizontalPadding: 7
      ),
      client: SpoolClient(),
      iconProvider: AppIconProvider()
    )

    XCTAssertEqual(view.renderedWindowIDs, [30, 10, 20, 40])
  }

  func testOverflowNavigationStopsAndHidesArrowsAtBothEnds() throws {
    let document = try state(windowIDs: [10, 20, 30, 40, 50])
    let view = WorkspaceBarView(
      displayID: 1,
      workspaces: document.spaces,
      preferences: BarPreferences(
        barHeight: 28,
        itemSpacing: 5,
        windowSpacing: 3,
        horizontalPadding: 7,
        animationStyle: .none
      ),
      client: SpoolClient(),
      iconProvider: AppIconProvider()
    )
    view.frame = CGRect(x: 0, y: 0, width: 150, height: 28)
    view.layoutSubtreeIfNeeded()

    XCTAssertTrue(view.renderedWindowNavigationVisible)
    XCTAssertFalse(view.renderedPreviousWindowNavigationVisible)
    XCTAssertTrue(view.renderedNextWindowNavigationVisible)
    XCTAssertEqual(view.renderedVisibleWindowIDs, [10, 20])
    let buttons = descendants(of: view, as: BarActionButton.self)
    let next = buttons.first { $0.identifier == WorkspaceBarView.nextWindowButtonID }
    let previous = buttons.first { $0.identifier == WorkspaceBarView.previousWindowButtonID }
    next?.performClick(nil)
    XCTAssertEqual(view.renderedVisibleWindowIDs, [20, 30])
    XCTAssertTrue(view.renderedPreviousWindowNavigationVisible)
    XCTAssertTrue(view.renderedNextWindowNavigationVisible)
    previous?.performClick(nil)
    XCTAssertEqual(view.renderedVisibleWindowIDs, [10, 20])
    XCTAssertEqual(view.rotateWindows(step: -1), [10, 20])

    XCTAssertEqual(view.rotateWindows(step: 1), [20, 30])
    XCTAssertEqual(view.rotateWindows(step: 1), [30, 40])
    XCTAssertEqual(view.rotateWindows(step: 1), [40, 50])
    XCTAssertFalse(view.renderedNextWindowNavigationVisible)
    XCTAssertTrue(view.renderedPreviousWindowNavigationVisible)
    XCTAssertEqual(view.rotateWindows(step: 1), [40, 50])

    view.frame.size.width = 300
    view.layoutSubtreeIfNeeded()
    XCTAssertFalse(view.renderedWindowNavigationVisible)
    XCTAssertEqual(view.renderedVisibleWindowIDs, [10, 20, 30, 40, 50])
  }

  func testWindowArrowPushesIconsInTheNavigationDirection() throws {
    let document = try state(windowIDs: [10, 20, 30, 40, 50], focusedWindowID: 20)
    let preferences = BarPreferences(animationStyle: .smooth, animationDuration: 0.24)
    let view = WorkspaceBarView(
      displayID: 1,
      workspaces: document.spaces,
      preferences: preferences,
      client: SpoolClient(),
      iconProvider: AppIconProvider(),
      reduceMotion: false
    )
    view.frame = CGRect(x: 0, y: 0, width: 150, height: 28)
    view.layoutSubtreeIfNeeded()
    let initialFocusFrame = try XCTUnwrap(view.renderedFocusRingFrame)

    XCTAssertTrue(
      view.renderedWindowMotionIsClipped,
      "The animated icon layer must be clipped by a separate viewport layer"
    )
    XCTAssertEqual(view.rotateWindows(step: 1), [20, 30])
    XCTAssertEqual(view.renderedWindowMotionDirection, .forward)
    XCTAssertNotNil(view.renderedFocusTransition as? CAAnimationGroup)
    XCTAssertLessThan(try XCTUnwrap(view.renderedFocusRingFrame).minX, initialFocusFrame.minX)
    let spec = try XCTUnwrap(
      BarMotionSpec.resolve(
        style: preferences.animationStyle,
        duration: preferences.animationDuration,
        reduceMotion: false
      )
    )
    let forward = BarMotion.pushAnimation(direction: .forward, spec: spec)
    XCTAssertEqual(forward?.type, .push)
    XCTAssertEqual(forward?.subtype, .fromRight)
    XCTAssertEqual(forward?.duration, preferences.animationDuration)

    XCTAssertEqual(view.rotateWindows(step: -1), [10, 20])
    XCTAssertEqual(view.renderedWindowMotionDirection, .backward)
    let backward = BarMotion.pushAnimation(direction: .backward, spec: spec)
    XCTAssertEqual(backward?.subtype, .fromLeft)
  }

  func testWorkspaceStateChangePushesContentTowardTheSelectedWorkspace() throws {
    let controller = RecordingSpoolController()
    let first = try state(workspaces: [
      (number: 1, windowIDs: [10], selected: true),
      (number: 2, windowIDs: [20], selected: false),
    ])
    let view = WorkspaceBarView(
      displayID: 7,
      workspaces: first.spaces,
      preferences: BarPreferences(animationStyle: .spring, animationDuration: 0.24),
      client: controller,
      iconProvider: AppIconProvider(),
      reduceMotion: false
    )
    view.frame = CGRect(x: 0, y: 0, width: 180, height: 28)
    view.layoutSubtreeIfNeeded()

    XCTAssertEqual(view.cycleWorkspace(step: 1), 2)
    let second = try state(workspaces: [
      (number: 1, windowIDs: [10], selected: false),
      (number: 2, windowIDs: [20], selected: true),
    ])
    view.update(workspaces: second.spaces)

    XCTAssertEqual(view.renderedWorkspaceMotionDirection, .forward)
    XCTAssertTrue(
      view.renderedWorkspaceMotionIsClipped,
      "The animated workspace label must be clipped by a separate viewport"
    )
  }

  func testFocusedWindowIndicatorGlidesBetweenVisibleIcons() throws {
    let first = try state(windowIDs: [10, 20], focusedWindowID: 10)
    let view = WorkspaceBarView(
      displayID: 1,
      workspaces: first.spaces,
      preferences: BarPreferences(animationStyle: .smooth),
      client: SpoolClient(),
      iconProvider: AppIconProvider(),
      reduceMotion: false
    )
    view.frame = CGRect(x: 0, y: 0, width: 180, height: 28)
    view.layoutSubtreeIfNeeded()
    let firstFrame = try XCTUnwrap(view.renderedFocusRingFrame)

    let second = try state(windowIDs: [10, 20], focusedWindowID: 20)
    view.update(workspaces: second.spaces)

    XCTAssertNotNil(view.renderedFocusTransition as? CAAnimationGroup)
    XCTAssertGreaterThan(try XCTUnwrap(view.renderedFocusRingFrame).minX, firstFrame.minX)
  }

  func testFocusedWindowOutsideViewportScrollsIntoView() throws {
    let first = try state(windowIDs: [10, 20, 30, 40, 50], focusedWindowID: 10)
    let view = WorkspaceBarView(
      displayID: 1,
      workspaces: first.spaces,
      preferences: BarPreferences(animationStyle: .smooth),
      client: SpoolClient(),
      iconProvider: AppIconProvider(),
      reduceMotion: false
    )
    view.frame = CGRect(x: 0, y: 0, width: 150, height: 28)
    view.layoutSubtreeIfNeeded()
    XCTAssertEqual(view.renderedVisibleWindowIDs, [10, 20])

    let last = try state(windowIDs: [10, 20, 30, 40, 50], focusedWindowID: 50)
    view.update(workspaces: last.spaces)

    XCTAssertEqual(view.renderedVisibleWindowIDs, [40, 50])
    XCTAssertEqual(view.renderedWindowMotionDirection, .forward)
    XCTAssertNotNil(view.renderedFocusRingFrame)
    XCTAssertNotNil(view.renderedFocusTransition as? CAAnimationGroup)
  }

  func testReduceMotionDisablesWorkspaceAndWindowTransitions() throws {
    XCTAssertNil(
      BarMotionSpec.resolve(style: .spring, duration: 0.24, reduceMotion: true)
    )
    let document = try state(windowIDs: [10, 20, 30, 40, 50])
    let view = WorkspaceBarView(
      displayID: 1,
      workspaces: document.spaces,
      preferences: BarPreferences(animationStyle: .spring),
      client: SpoolClient(),
      iconProvider: AppIconProvider(),
      reduceMotion: true
    )
    view.frame = CGRect(x: 0, y: 0, width: 150, height: 28)
    view.layoutSubtreeIfNeeded()

    _ = view.rotateWindows(step: 1)
    XCTAssertNil(view.renderedWindowScrollTransition)
    XCTAssertNil(view.renderedWindowMotionDirection)
  }

  func testConfigurationBubbleIsCenteredBelowItsAnchor() {
    let placement = ConfigurationBubblePlacement.place(
      below: CGRect(x: 490, y: 850, width: 20, height: 28),
      in: CGRect(x: 0, y: 0, width: 1_000, height: 900),
      panelSize: CGSize(width: 380, height: 520)
    )

    XCTAssertEqual(placement.frame.origin.x, 310)
    XCTAssertEqual(placement.frame.origin.y, 326)
    XCTAssertEqual(placement.arrowX, 190)
  }

  func testConfigurationBubbleStaysInsideScreenAndArrowTracksAnchor() {
    let placement = ConfigurationBubblePlacement.place(
      below: CGRect(x: 970, y: 850, width: 20, height: 28),
      in: CGRect(x: 0, y: 0, width: 1_000, height: 900),
      panelSize: CGSize(width: 380, height: 520)
    )

    XCTAssertEqual(placement.frame.maxX, 992)
    XCTAssertEqual(placement.arrowX, 362)
  }

  func testConfigurationBubbleClampsAboveVisibleFrameBottom() {
    let placement = ConfigurationBubblePlacement.place(
      below: CGRect(x: 490, y: 300, width: 20, height: 28),
      in: CGRect(x: 0, y: 40, width: 1_000, height: 860),
      panelSize: CGSize(width: 380, height: 520)
    )

    XCTAssertEqual(placement.frame.minY, 48)
  }

  func testFocusRingIsPositionedAfterViewReceivesItsRealFrame() throws {
    let document = try state(windowIDs: [10], focusedWindowID: 10)
    let preferences = BarPreferences(animationStyle: .none)
    let view = WorkspaceBarView(
      displayID: 1,
      workspaces: document.spaces,
      preferences: preferences,
      client: SpoolClient(),
      iconProvider: AppIconProvider()
    )

    view.frame = CGRect(x: 0, y: 0, width: 120, height: preferences.barHeight)
    view.layoutSubtreeIfNeeded()
    let button = descendants(of: view, as: WindowActionButton.self).first
    let expected = button.map {
      view.convert($0.bounds, from: $0).insetBy(dx: -1, dy: -1)
    }
    XCTAssertEqual(view.renderedFocusRingFrame, expected)
    XCTAssertEqual(
      view.renderedFocusFillColor,
      NSColor(spoolHex: preferences.selectionColorHex)?.withAlphaComponent(0.16).cgColor
    )
    XCTAssertEqual(view.renderedFocusIndicatorFrame.height, preferences.focusRingWidth)
  }

  func testConfigurationPanelCanActivateForSystemColorPicker() {
    XCTAssertFalse(
      ConfigurationPanelController.panelStyleMask.contains(.nonactivatingPanel)
    )
  }

  func testColorSerializationPreservesAlpha() {
    let color = NSColor(srgbRed: 1, green: 0.5, blue: 0, alpha: 0.25)
    XCTAssertEqual(color.spoolHexString, "#FF800040")
  }

  private func state(
    windowIDs: [Int32],
    floatingWindowIDs: Set<Int32> = [],
    focusedWindowID: Int32? = nil,
    selected: Bool = true
  ) throws -> SpoolStateDocument {
    let windows: [[String: Any]] = windowIDs.map { windowID in
      [
        "window_id": windowID,
        "bundle_id": "",
        "app_name": "App \(windowID)",
        "title": "Window \(windowID)",
        "focused": windowID == focusedWindowID,
        "floating": floatingWindowIDs.contains(windowID),
        "display_id": 1,
        "visible": true,
      ]
    }
    let payload: [String: Any] = [
      "version": 2,
      "timestamp": 1,
      "active": [
        "display_id": 1,
        "native_workspace_id": 10,
        "virtual_workspace_number": 1,
      ],
      "displays": [
        [
          "display_id": 1,
          "active": true,
          "native_workspace_id": 10,
          "virtual_workspace_number": 1,
        ]
      ],
      "virtual_workspaces": [
        [
          "number": 1,
          "native_workspace_id": 10,
          "display_id": 1,
          "selected": selected,
          "active": selected,
          "windows": windows,
        ]
      ],
    ]
    let data = try JSONSerialization.data(withJSONObject: payload)
    return try JSONDecoder().decode(SpoolStateDocument.self, from: data)
  }

  private func state(
    workspaces: [(number: UInt32, windowIDs: [Int32], selected: Bool)]
  ) throws -> SpoolStateDocument {
    let rows: [[String: Any]] = workspaces.map { workspace in
      [
        "number": workspace.number,
        "native_workspace_id": 10,
        "display_id": 1,
        "selected": workspace.selected,
        "active": workspace.selected,
        "windows": workspace.windowIDs.map { windowID in
          [
            "window_id": windowID,
            "bundle_id": "",
            "app_name": "App \(windowID)",
            "title": "Window \(windowID)",
            "focused": false,
            "floating": false,
            "display_id": 1,
            "visible": true,
          ] as [String: Any]
        },
      ]
    }
    let selected = workspaces.first(where: \.selected)?.number
    let payload: [String: Any] = [
      "version": 2,
      "timestamp": 1,
      "active": [
        "display_id": 1,
        "native_workspace_id": 10,
        "virtual_workspace_number": selected as Any,
      ],
      "displays": [
        [
          "display_id": 1,
          "active": true,
          "native_workspace_id": 10,
          "virtual_workspace_number": selected as Any,
        ]
      ],
      "virtual_workspaces": rows,
    ]
    let data = try JSONSerialization.data(withJSONObject: payload)
    return try JSONDecoder().decode(SpoolStateDocument.self, from: data)
  }

  private func notchedGeometry(trayFrames: [CGRect]) -> ScreenGeometry {
    ScreenGeometry(
      frame: CGRect(x: 0, y: 0, width: 1000, height: 700),
      visibleFrame: CGRect(x: 0, y: 0, width: 1000, height: 670),
      safeTopInset: 30,
      auxiliaryLeft: CGRect(x: 0, y: 670, width: 430, height: 30),
      auxiliaryRight: CGRect(x: 570, y: 670, width: 430, height: 30),
      trayFrames: trayFrames
    )
  }

  private func descendants<T: NSView>(of root: NSView, as type: T.Type) -> [T] {
    root.subviews.flatMap { view in
      (view as? T).map { [$0] } ?? descendants(of: view, as: type)
    }
  }
}

private final class RecordingSpoolController: SpoolControlling {
  struct Move: Equatable {
    let windowID: Int32
    let displayID: UInt32
    let workspaceNumber: UInt32
    let follow: Bool
  }

  struct WorkspaceSelection: Equatable {
    let displayID: UInt32
    let number: UInt32
  }

  var focusedWindowIDs: [Int32] = []
  var moves: [Move] = []
  var selectedWorkspaces: [WorkspaceSelection] = []

  func requestRefresh() {}
  func focus(windowID: Int32) { focusedWindowIDs.append(windowID) }
  func selectWorkspace(displayID: UInt32, number: UInt32) {
    selectedWorkspaces.append(.init(displayID: displayID, number: number))
  }
  func moveWindow(
    windowID: Int32,
    displayID: UInt32,
    workspaceNumber: UInt32,
    follow: Bool
  ) {
    moves.append(
      .init(
        windowID: windowID,
        displayID: displayID,
        workspaceNumber: workspaceNumber,
        follow: follow
      ))
  }
}
