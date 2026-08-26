import AppKit
import QuartzCore

final class WorkspaceBarView: NSView, NSMenuDelegate {
  private static let workspaceFont = NSFont.monospacedDigitSystemFont(
    ofSize: 12,
    weight: .semibold
  )
  private static let navigationButtonWidth: CGFloat = 16
  static let previousWorkspaceButtonID = NSUserInterfaceItemIdentifier(
    "SpoolBar.previousWorkspace")
  static let nextWorkspaceButtonID = NSUserInterfaceItemIdentifier("SpoolBar.nextWorkspace")
  static let previousWindowButtonID = NSUserInterfaceItemIdentifier("SpoolBar.previousWindow")
  static let nextWindowButtonID = NSUserInterfaceItemIdentifier("SpoolBar.nextWindow")

  private let displayID: UInt32
  private let preferences: BarPreferences
  private weak var client: (any SpoolControlling)?
  private let iconProvider: AppIconProvider
  private let onSettingsRequested: ((CGRect, NSScreen?) -> Void)?
  private let isBarLocked: () -> Bool
  private let onBarLockChanged: ((Bool) -> Void)?
  private let motionSpec: BarMotionSpec?
  private var lockMenuItem: NSMenuItem?
  private let previousWorkspaceButton = BarActionButton()
  private let workspaceViewport = WorkspaceViewportView()
  private let workspaceButton = BarActionButton()
  private let nextWorkspaceButton = BarActionButton()
  private let sectionSeparator = BarSeparatorView()
  private let previousWindowButton = BarActionButton()
  private let windowViewport = WindowViewportView()
  private let nextWindowButton = BarActionButton()
  private let focusRing = FocusRingView()
  private var workspaces: [VirtualWorkspaceState] = []
  private var workspaceNumbers: [UInt32] = []
  private var activeWorkspaceNumber: UInt32?
  private var windowButtons: [Int32: WindowActionButton] = [:]
  private var windowOrder: [Int32] = []
  private var focusedWindowID: Int32?
  private var pendingWorkspaceTransition: (target: UInt32, direction: BarMotionDirection)?
  private var pendingFocusSourceFrame: CGRect?
  private var lastWorkspaceMotionDirection: BarMotionDirection?
  private var lastWindowMotionDirection: BarMotionDirection?
  private var windowOffset = 0
  private var windowCapacity = 0
  private var visibleWindowIDs: [Int32] = []
  private var workspaceScrollAccumulator: CGFloat = 0
  private var windowScrollAccumulator: CGFloat = 0

  init(
    displayID: UInt32,
    workspaces: [VirtualWorkspaceState],
    preferences: BarPreferences,
    client: any SpoolControlling,
    iconProvider: AppIconProvider,
    onSettingsRequested: ((CGRect, NSScreen?) -> Void)? = nil,
    isBarLocked: @escaping () -> Bool = { false },
    onBarLockChanged: ((Bool) -> Void)? = nil,
    reduceMotion: Bool = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
  ) {
    self.displayID = displayID
    self.preferences = preferences
    self.client = client
    self.iconProvider = iconProvider
    self.onSettingsRequested = onSettingsRequested
    self.isBarLocked = isBarLocked
    self.onBarLockChanged = onBarLockChanged
    self.motionSpec = BarMotionSpec.resolve(
      style: preferences.animationStyle,
      duration: preferences.animationDuration,
      reduceMotion: reduceMotion
    )
    super.init(frame: .zero)
    build()
    update(workspaces: workspaces)
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }

  static func estimatedWidth(
    workspace: VirtualWorkspaceState,
    preferences: BarPreferences
  ) -> CGFloat {
    let label = preferences.workspaceLabel(for: workspace.number)
    return WorkspaceBarLayout.estimatedWidth(
      workspaceButtonWidth: workspaceButtonWidth(label: label),
      windowCount: workspace.windows.count,
      metrics: layoutMetrics(preferences)
    )
  }

  static func minimumWidth(
    workspace: VirtualWorkspaceState,
    preferences: BarPreferences
  ) -> CGFloat {
    let label = preferences.workspaceLabel(for: workspace.number)
    return WorkspaceBarLayout.minimumWidth(
      workspaceButtonWidth: workspaceButtonWidth(label: label),
      windowCount: workspace.windows.count,
      metrics: layoutMetrics(preferences)
    )
  }

  var renderedWindowIDs: [Int32] { windowOrder }
  var renderedVisibleWindowIDs: [Int32] { visibleWindowIDs }
  var renderedFocusRingFrame: CGRect? { focusRing.isHidden ? nil : focusRing.frame }
  var renderedFocusFillColor: CGColor? { focusRing.renderedFillColor }
  var renderedFocusIndicatorFrame: CGRect { focusRing.renderedIndicatorFrame }
  var renderedWorkspaceTransition: CAAnimation? {
    workspaceButton.layer?.animation(forKey: BarMotion.workspaceKey)
  }
  var renderedWindowScrollTransition: CAAnimation? {
    windowViewport.motionLayer?.animation(forKey: BarMotion.windowScrollKey)
  }
  var renderedFocusTransition: CAAnimation? {
    focusRing.layer?.animation(forKey: BarMotion.focusKey)
  }
  var renderedWorkspaceMotionDirection: BarMotionDirection? { lastWorkspaceMotionDirection }
  var renderedWindowMotionDirection: BarMotionDirection? { lastWindowMotionDirection }
  var renderedWorkspaceMotionIsClipped: Bool {
    guard let container = workspaceButton.superview, container !== self else { return false }
    return container.layer?.masksToBounds == true
  }
  var renderedWindowMotionIsClipped: Bool { windowViewport.motionContentIsClipped }
  var renderedPreviousWorkspaceNavigationVisible: Bool { !previousWorkspaceButton.isHidden }
  var renderedNextWorkspaceNavigationVisible: Bool { !nextWorkspaceButton.isHidden }
  var renderedPreviousWindowNavigationVisible: Bool { !previousWindowButton.isHidden }
  var renderedNextWindowNavigationVisible: Bool { !nextWindowButton.isHidden }
  var renderedWindowNavigationVisible: Bool {
    renderedPreviousWindowNavigationVisible || renderedNextWindowNavigationVisible
  }

  var settingsButtonScreenRect: CGRect? {
    guard let window else { return nil }
    layoutSubtreeIfNeeded()
    let rectInWindow = workspaceButton.convert(workspaceButton.bounds, to: nil)
    return window.convertToScreen(rectInWindow)
  }

  override func layout() {
    super.layout()
    let label =
      activeWorkspaceNumber.flatMap { number in
        preferences.workspaceLabel(for: number)
      } ?? "-"
    let resolved = WorkspaceBarLayout.resolve(
      WorkspaceBarLayoutInput(
        bounds: bounds,
        workspaceButtonWidth: Self.workspaceButtonWidth(label: label),
        windowCount: windowOrder.count,
        windowOffset: windowOffset,
        metrics: Self.layoutMetrics(preferences)
      )
    )
    previousWorkspaceButton.frame = resolved.previousWorkspaceFrame
    workspaceViewport.frame = resolved.workspaceFrame
    workspaceButton.frame = workspaceViewport.bounds
    nextWorkspaceButton.frame = resolved.nextWorkspaceFrame
    sectionSeparator.frame = resolved.separatorFrame ?? .zero
    sectionSeparator.isHidden = resolved.separatorFrame == nil
    previousWindowButton.frame = resolved.previousWindowFrame
    windowViewport.frame = resolved.windowViewportFrame
    nextWindowButton.frame = resolved.nextWindowFrame
    windowCapacity = resolved.windowCapacity
    windowOffset = resolved.windowOffset
    layoutWindowButtons(visibleRange: resolved.visibleWindowRange)
    previousWindowButton.isHidden = !resolved.canPageWindowsBackward
    nextWindowButton.isHidden = !resolved.canPageWindowsForward
    previousWindowButton.isEnabled = !previousWindowButton.isHidden
    nextWindowButton.isEnabled = !nextWindowButton.isHidden
    updateFocusRing()
  }

  func update(workspaces: [VirtualWorkspaceState]) {
    self.workspaces = workspaces.sorted { $0.number < $1.number }
    workspaceNumbers = self.workspaces.map(\.number)
    guard let active = self.workspaces.first(where: \.selected) ?? self.workspaces.first else {
      activeWorkspaceNumber = nil
      previousWorkspaceButton.isHidden = true
      nextWorkspaceButton.isHidden = true
      windowOrder = []
      windowButtons.values.forEach { $0.removeFromSuperview() }
      windowButtons.removeAll()
      needsLayout = true
      return
    }

    let previousWorkspaceNumber = activeWorkspaceNumber
    let workspaceChanged = previousWorkspaceNumber != active.number
    if let previousWorkspaceNumber, workspaceChanged {
      let direction =
        pendingWorkspaceTransition.flatMap { pending in
          pending.target == active.number ? pending.direction : nil
        }
        ?? (active.number > previousWorkspaceNumber ? .forward : .backward)
      prepareWorkspaceTransition(direction: direction)
      pendingWorkspaceTransition = nil
    }
    activeWorkspaceNumber = active.number
    updateWorkspaceNavigation()
    workspaceButton.workspaceNumber = active.number
    let label = preferences.workspaceLabel(for: active.number)
    workspaceButton.title = label
    workspaceButton.toolTip = "Workspace \(active.number). Scroll to switch."
    workspaceButton.setSelectedAppearance(
      fill: NSColor(spoolHex: preferences.activeWorkspaceColorHex) ?? .clear,
      accent: NSColor(spoolHex: preferences.selectionColorHex) ?? .controlAccentColor
    )

    let orderedWindows = active.displayOrderedWindows
    let nextFocusedWindowID = orderedWindows.first(where: \.focused)?.windowID
    let focusChanged = focusedWindowID != nextFocusedWindowID
    for window in orderedWindows {
      let button = windowButtons[window.windowID] ?? makeWindowButton(window)
      update(button, with: window)
      windowButtons[window.windowID] = button
    }
    let desiredOrder = orderedWindows.map(\.windowID)
    let desired = Set(desiredOrder)
    for windowID in windowButtons.keys.filter({ !desired.contains($0) }) {
      windowButtons.removeValue(forKey: windowID)?.removeFromSuperview()
    }
    for windowID in desiredOrder where windowButtons[windowID]?.superview == nil {
      if let button = windowButtons[windowID] {
        windowViewport.addContentSubview(button)
      }
    }
    windowOrder = desiredOrder
    if workspaceChanged {
      windowOffset = 0
    } else {
      windowOffset = min(windowOffset, max(0, windowOrder.count - 1))
    }
    var scrolledForFocus = false
    if !workspaceChanged,
      focusChanged,
      windowCapacity > 0,
      let nextFocusedWindowID,
      let focusedIndex = windowOrder.firstIndex(of: nextFocusedWindowID)
    {
      let previousOffset = windowOffset
      if focusedIndex < windowOffset {
        windowOffset = focusedIndex
      } else if focusedIndex >= windowOffset + windowCapacity {
        windowOffset = focusedIndex - windowCapacity + 1
      }
      if windowOffset != previousOffset {
        let direction: BarMotionDirection = windowOffset > previousOffset ? .forward : .backward
        captureFocusSourceFrame()
        prepareWindowScroll(direction: direction)
        scrolledForFocus = true
      }
    }
    if !workspaceChanged, focusChanged, !scrolledForFocus, !focusRing.isHidden {
      captureFocusSourceFrame()
    }
    focusedWindowID = nextFocusedWindowID
    needsLayout = true
    layoutSubtreeIfNeeded()
  }

  @discardableResult
  func cycleWorkspace(step: Int) -> UInt32? {
    guard let current = activeWorkspaceNumber,
      let target = WorkspaceCycle.target(
        numbers: workspaceNumbers,
        current: current,
        step: step
      )
    else { return nil }
    pendingWorkspaceTransition = (target, BarMotionDirection(step: step))
    client?.selectWorkspace(displayID: displayID, number: target)
    return target
  }

  @discardableResult
  func rotateWindows(step: Int) -> [Int32] {
    let maximumOffset = max(0, windowOrder.count - windowCapacity)
    guard maximumOffset > 0, step != 0
    else { return visibleWindowIDs }
    let nextOffset = min(max(0, windowOffset + (step > 0 ? 1 : -1)), maximumOffset)
    guard nextOffset != windowOffset else { return visibleWindowIDs }
    captureFocusSourceFrame()
    prepareWindowScroll(direction: BarMotionDirection(step: step))
    windowOffset = nextOffset
    needsLayout = true
    layoutSubtreeIfNeeded()
    return visibleWindowIDs
  }

  @discardableResult
  func moveWindowToWorkspace(_ windowID: Int32) -> Bool {
    guard !windowOrder.contains(windowID),
      let activeWorkspaceNumber,
      let client
    else { return false }
    client.moveWindow(
      windowID: windowID,
      displayID: displayID,
      workspaceNumber: activeWorkspaceNumber,
      follow: true
    )
    return true
  }

  private func build() {
    wantsLayer = true
    layer?.backgroundColor = NSColor.clear.cgColor
    registerForDraggedTypes([.spoolWindowID])

    configureNavigationButton(
      previousWorkspaceButton,
      symbolName: "chevron.left",
      toolTip: "Previous workspace",
      identifier: Self.previousWorkspaceButtonID,
      action: #selector(previousWorkspaceClicked)
    )
    addSubview(previousWorkspaceButton)

    workspaceButton.target = nil
    workspaceButton.action = nil
    workspaceButton.allowsCommandInteraction = false
    workspaceButton.motionSpec = motionSpec
    workspaceButton.onScroll = { [weak self] event in self?.handleWorkspaceScroll(event) }
    workspaceButton.isBordered = false
    workspaceButton.font = Self.workspaceFont
    workspaceButton.wantsLayer = true
    workspaceButton.layer?.cornerRadius = 4
    workspaceViewport.addSubview(workspaceButton)
    addSubview(workspaceViewport)

    configureNavigationButton(
      nextWorkspaceButton,
      symbolName: "chevron.right",
      toolTip: "Next workspace",
      identifier: Self.nextWorkspaceButtonID,
      action: #selector(nextWorkspaceClicked)
    )
    addSubview(nextWorkspaceButton)

    sectionSeparator.isHidden = true
    addSubview(sectionSeparator)

    configureNavigationButton(
      previousWindowButton,
      symbolName: "chevron.left",
      toolTip: "Previous window",
      identifier: Self.previousWindowButtonID,
      action: #selector(previousWindowClicked)
    )
    previousWindowButton.isHidden = true
    addSubview(previousWindowButton)

    windowViewport.wantsLayer = true
    windowViewport.layer?.masksToBounds = true
    windowViewport.onScroll = { [weak self] event in self?.handleWindowScroll(event) }
    addSubview(windowViewport)

    configureNavigationButton(
      nextWindowButton,
      symbolName: "chevron.right",
      toolTip: "Next window",
      identifier: Self.nextWindowButtonID,
      action: #selector(nextWindowClicked)
    )
    nextWindowButton.isHidden = true
    addSubview(nextWindowButton)

    focusRing.wantsLayer = true
    focusRing.layer?.cornerRadius = 5
    focusRing.layer?.backgroundColor = NSColor.clear.cgColor
    focusRing.isHidden = true
    addSubview(focusRing, positioned: .above, relativeTo: windowViewport)

    let menu = NSMenu()
    let lockItem = menu.addItem(
      withTitle: "Lock Bar",
      action: #selector(toggleBarLock),
      keyEquivalent: ""
    )
    lockItem.image = NSImage(systemSymbolName: "lock", accessibilityDescription: nil)
    lockMenuItem = lockItem
    menu.addItem(.separator())
    let settingsItem = menu.addItem(
      withTitle: "Settings",
      action: #selector(settingsClicked),
      keyEquivalent: ","
    )
    settingsItem.target = self
    settingsItem.image = NSImage(
      systemSymbolName: "gearshape",
      accessibilityDescription: nil
    )
    menu.addItem(.separator())
    let refreshItem = menu.addItem(
      withTitle: "Refresh",
      action: #selector(refresh),
      keyEquivalent: ""
    )
    refreshItem.image = NSImage(systemSymbolName: "arrow.clockwise", accessibilityDescription: nil)
    menu.addItem(.separator())
    let quitItem = menu.addItem(
      withTitle: "Quit SpoolBar",
      action: #selector(quit),
      keyEquivalent: "q"
    )
    quitItem.image = NSImage(systemSymbolName: "power", accessibilityDescription: nil)
    for item in menu.items { item.target = self }
    menu.delegate = self
    self.menu = menu
  }

  func menuWillOpen(_ menu: NSMenu) {
    let locked = isBarLocked()
    lockMenuItem?.title = locked ? "Unlock Bar" : "Lock Bar"
    lockMenuItem?.image = NSImage(
      systemSymbolName: locked ? "lock.open" : "lock",
      accessibilityDescription: nil
    )
  }

  private func makeWindowButton(_ window: WindowState) -> WindowActionButton {
    let button = WindowActionButton()
    button.motionSpec = motionSpec
    button.target = self
    button.action = #selector(windowClicked(_:))
    button.onScroll = { [weak self] event in self?.handleWindowScroll(event) }
    button.isBordered = false
    button.wantsLayer = true
    button.layer?.cornerRadius = 4
    update(button, with: window)
    return button
  }

  private func configureNavigationButton(
    _ button: BarActionButton,
    symbolName: String,
    toolTip: String,
    identifier: NSUserInterfaceItemIdentifier,
    action: Selector
  ) {
    button.identifier = identifier
    button.motionSpec = motionSpec
    button.target = self
    button.action = action
    button.title = ""
    let symbol = NSImage(systemSymbolName: symbolName, accessibilityDescription: toolTip)
    button.image = symbol?.withSymbolConfiguration(
      NSImage.SymbolConfiguration(pointSize: 8, weight: .semibold)
    )
    button.imagePosition = .imageOnly
    button.imageScaling = .scaleProportionallyDown
    button.contentTintColor = .secondaryLabelColor
    button.toolTip = toolTip
    button.isBordered = false
    button.setAccessibilityLabel(toolTip)
  }

  private func update(_ button: WindowActionButton, with window: WindowState) {
    button.windowID = window.windowID
    let iconKey = window.bundleID.isEmpty ? window.appName : window.bundleID
    if button.iconKey != iconKey {
      button.iconKey = iconKey
      button.setIcon(iconProvider.icon(bundleID: window.bundleID, appName: window.appName))
    }
    button.toolTip = window.title.isEmpty ? window.appName : window.title
    if button.windowFloating != window.floating {
      button.windowFloating = window.floating
      applyFloatingStyle(to: button, floating: window.floating)
    }
  }

  private func layoutWindowButtons(visibleRange: Range<Int>) {
    let side = preferences.iconButtonSize
    let stride = side + preferences.windowSpacing
    visibleWindowIDs = visibleRange.map { windowOrder[$0] }
    let positions = Dictionary(
      uniqueKeysWithValues: visibleWindowIDs.enumerated().map {
        ($0.element, $0.offset)
      })
    for windowID in windowOrder {
      guard let button = windowButtons[windowID] else { continue }
      guard let index = positions[windowID] else {
        button.isHidden = true
        continue
      }
      button.isHidden = false
      button.frame = CGRect(x: CGFloat(index) * stride, y: 0, width: side, height: side)
      button.layer?.zPosition = 0
      button.stackLayer.setAffineTransform(.identity)
    }
  }

  private func updateFocusRing() {
    guard preferences.showsFocusRing,
      let focusedWindowID,
      visibleWindowIDs.contains(focusedWindowID),
      let button = windowButtons[focusedWindowID],
      !button.isHidden
    else {
      pendingFocusSourceFrame = nil
      focusRing.isHidden = true
      return
    }
    let color = NSColor(spoolHex: preferences.selectionColorHex) ?? .controlAccentColor
    focusRing.apply(color: color, indicatorHeight: preferences.focusRingWidth)
    let ringOutset = ceil(preferences.focusRingWidth / 2)
    let targetFrame = convert(button.bounds, from: button).insetBy(
      dx: -ringOutset,
      dy: -ringOutset
    )
    let sourceFrame = pendingFocusSourceFrame
    pendingFocusSourceFrame = nil
    let wasHidden = focusRing.isHidden
    focusRing.frame = targetFrame
    focusRing.isHidden = false
    if let sourceFrame {
      BarMotion.addFocusMove(
        to: focusRing.layer,
        from: sourceFrame,
        to: targetFrame,
        spec: motionSpec
      )
    } else if wasHidden {
      BarMotion.addFade(to: focusRing.layer, spec: motionSpec, key: BarMotion.focusFadeKey)
    }
  }

  private func prepareWorkspaceTransition(direction: BarMotionDirection) {
    lastWorkspaceMotionDirection = motionSpec == nil ? nil : direction
    BarMotion.addPush(
      to: workspaceButton.layer,
      direction: direction,
      spec: motionSpec,
      key: BarMotion.workspaceKey
    )
    BarMotion.addPush(
      to: windowViewport.motionLayer,
      direction: direction,
      spec: motionSpec,
      key: BarMotion.workspaceKey
    )
    BarMotion.addFade(to: focusRing.layer, spec: motionSpec, key: BarMotion.focusFadeKey)
    pendingFocusSourceFrame = nil
  }

  private func prepareWindowScroll(direction: BarMotionDirection) {
    lastWindowMotionDirection = motionSpec == nil ? nil : direction
    BarMotion.addPush(
      to: windowViewport.motionLayer,
      direction: direction,
      spec: motionSpec,
      key: BarMotion.windowScrollKey
    )
  }

  private func captureFocusSourceFrame() {
    guard !focusRing.isHidden else { return }
    pendingFocusSourceFrame = focusRing.layer?.presentation()?.frame ?? focusRing.frame
  }

  private func applyFloatingStyle(to button: WindowActionButton, floating: Bool) {
    button.floatingBadge?.removeFromSuperlayer()
    button.floatingBadge = nil
    button.alphaValue = 1
    button.stackLayer.shadowOpacity = 0
    button.stackLayer.shadowRadius = 0
    button.stackLayer.shadowOffset = .zero
    guard floating else { return }

    switch preferences.floatingIconStyle {
    case .badge:
      let badge = CALayer()
      badge.backgroundColor = NSColor.systemOrange.cgColor
      badge.borderColor = NSColor.white.withAlphaComponent(0.9).cgColor
      badge.borderWidth = 1
      badge.cornerRadius = 3
      let side = preferences.iconButtonSize
      badge.frame = CGRect(x: side - 7, y: side - 7, width: 6, height: 6)
      button.stackLayer.addSublayer(badge)
      button.floatingBadge = badge
    case .raised:
      button.stackLayer.shadowColor = NSColor.black.cgColor
      button.stackLayer.shadowOpacity = 0.55
      button.stackLayer.shadowRadius = 2.5
      button.stackLayer.shadowOffset = CGSize(width: 0, height: -1)
    case .dimmed:
      button.alphaValue = 0.62
    }
  }

  private func handleWorkspaceScroll(_ event: NSEvent) {
    guard let step = scrollStep(event, accumulator: &workspaceScrollAccumulator) else { return }
    cycleWorkspace(step: -step)
  }

  private func updateWorkspaceNavigation() {
    guard let activeWorkspaceNumber,
      let index = workspaceNumbers.firstIndex(of: activeWorkspaceNumber)
    else {
      previousWorkspaceButton.isHidden = true
      nextWorkspaceButton.isHidden = true
      return
    }
    previousWorkspaceButton.isHidden = index == workspaceNumbers.startIndex
    nextWorkspaceButton.isHidden =
      index == workspaceNumbers.index(before: workspaceNumbers.endIndex)
    previousWorkspaceButton.isEnabled = !previousWorkspaceButton.isHidden
    nextWorkspaceButton.isEnabled = !nextWorkspaceButton.isHidden
  }

  private func handleWindowScroll(_ event: NSEvent) {
    guard let step = scrollStep(event, accumulator: &windowScrollAccumulator) else { return }
    rotateWindows(step: step)
  }

  private func scrollStep(_ event: NSEvent, accumulator: inout CGFloat) -> Int? {
    guard event.momentumPhase.isEmpty else { return nil }
    if event.phase == .began { accumulator = 0 }
    let delta = event.scrollingDeltaY != 0 ? event.scrollingDeltaY : event.scrollingDeltaX
    guard delta != 0 else { return nil }
    if event.hasPreciseScrollingDeltas {
      accumulator += delta
      guard abs(accumulator) >= 12 else { return nil }
      accumulator = 0
    }
    return delta < 0 ? 1 : -1
  }

  override func draggingEntered(_ sender: NSDraggingInfo) -> NSDragOperation {
    let operation = acceptedDragOperation(sender)
    setDropHighlighted(!operation.isEmpty)
    return operation
  }

  override func draggingUpdated(_ sender: NSDraggingInfo) -> NSDragOperation {
    acceptedDragOperation(sender)
  }

  override func draggingExited(_ sender: NSDraggingInfo?) {
    setDropHighlighted(false)
  }

  override func performDragOperation(_ sender: NSDraggingInfo) -> Bool {
    defer { setDropHighlighted(false) }
    guard let windowID = draggedWindowID(sender) else { return false }
    return moveWindowToWorkspace(windowID)
  }

  private func acceptedDragOperation(_ sender: NSDraggingInfo) -> NSDragOperation {
    guard let windowID = draggedWindowID(sender), !windowOrder.contains(windowID) else {
      return []
    }
    return .move
  }

  private func draggedWindowID(_ sender: NSDraggingInfo) -> Int32? {
    sender.draggingPasteboard.string(forType: .spoolWindowID).flatMap(Int32.init)
  }

  private func setDropHighlighted(_ highlighted: Bool) {
    layer?.borderColor = highlighted ? NSColor.controlAccentColor.cgColor : nil
    layer?.borderWidth = highlighted ? 1.5 : 0
  }

  @objc private func previousWorkspaceClicked() { cycleWorkspace(step: -1) }
  @objc private func nextWorkspaceClicked() { cycleWorkspace(step: 1) }
  @objc private func previousWindowClicked() { rotateWindows(step: -1) }
  @objc private func nextWindowClicked() { rotateWindows(step: 1) }

  @objc private func windowClicked(_ sender: BarActionButton) {
    guard let windowID = sender.windowID else { return }
    client?.focus(windowID: windowID)
  }

  @objc private func settingsClicked() {
    guard let anchor = settingsButtonScreenRect else { return }
    onSettingsRequested?(anchor, window?.screen)
  }

  @objc private func toggleBarLock() {
    onBarLockChanged?(!isBarLocked())
  }

  @objc private func refresh() { client?.requestRefresh() }
  @objc private func quit() { NSApplication.shared.terminate(nil) }

  private static func workspaceButtonWidth(label: String) -> CGFloat {
    let title = label as NSString
    return ceil(title.size(withAttributes: [.font: workspaceFont]).width) + 8
  }

  private static func layoutMetrics(_ preferences: BarPreferences) -> WorkspaceBarMetrics {
    WorkspaceBarMetrics(
      iconButtonSize: preferences.iconButtonSize,
      navigationButtonWidth: navigationButtonWidth,
      horizontalPadding: preferences.horizontalPadding,
      workspaceSpacing: preferences.itemSpacing,
      windowSpacing: preferences.windowSpacing
    )
  }
}
