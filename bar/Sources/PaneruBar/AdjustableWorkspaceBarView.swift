import AppKit

struct BarWindowState: Codable, Equatable {
  var frame: CGRect?
  var isLocked: Bool

  init(frame: CGRect? = nil, isLocked: Bool = false) {
    self.frame = frame
    self.isLocked = isLocked
  }
}

final class BarWindowStateStore {
  private let defaults: UserDefaults
  private let keyPrefix: String

  init(
    defaults: UserDefaults = .standard,
    keyPrefix: String = "PaneruBar.windowState"
  ) {
    self.defaults = defaults
    self.keyPrefix = keyPrefix
  }

  func state(for displayID: UInt32) -> BarWindowState {
    guard let data = defaults.data(forKey: key(for: displayID)),
      let state = try? JSONDecoder().decode(BarWindowState.self, from: data)
    else { return BarWindowState() }
    return state
  }

  func save(_ state: BarWindowState, for displayID: UInt32) {
    guard let data = try? JSONEncoder().encode(state) else { return }
    defaults.set(data, forKey: key(for: displayID))
  }

  private func key(for displayID: UInt32) -> String {
    "\(keyPrefix).\(displayID)"
  }
}

enum BarWindowGeometry {
  static func constrained(
    _ frame: CGRect,
    to bounds: CGRect,
    minimumSize: CGSize
  ) -> CGRect {
    let width = min(max(frame.width, minimumSize.width), bounds.width)
    let height = min(max(frame.height, minimumSize.height), bounds.height)
    let x = min(max(frame.minX, bounds.minX), bounds.maxX - width)
    let y = min(max(frame.minY, bounds.minY), bounds.maxY - height)
    return CGRect(x: x, y: y, width: width, height: height)
  }

  static func resized(
    _ frame: CGRect,
    delta: CGPoint,
    edges: BarResizeEdges = [.right, .bottom],
    within bounds: CGRect,
    minimumSize: CGSize
  ) -> CGRect {
    var minX = frame.minX
    var maxX = frame.maxX
    var minY = frame.minY
    var maxY = frame.maxY
    if edges.contains(.left) {
      minX = min(max(frame.minX + delta.x, bounds.minX), frame.maxX - minimumSize.width)
    }
    if edges.contains(.right) {
      maxX = max(min(frame.maxX + delta.x, bounds.maxX), frame.minX + minimumSize.width)
    }
    if edges.contains(.bottom) {
      minY = min(max(frame.minY + delta.y, bounds.minY), frame.maxY - minimumSize.height)
    }
    if edges.contains(.top) {
      maxY = max(min(frame.maxY + delta.y, bounds.maxY), frame.minY + minimumSize.height)
    }
    return CGRect(x: minX, y: minY, width: maxX - minX, height: maxY - minY)
  }
}

struct BarResizeEdges: OptionSet, Equatable {
  let rawValue: UInt8

  static let left = Self(rawValue: 1 << 0)
  static let right = Self(rawValue: 1 << 1)
  static let bottom = Self(rawValue: 1 << 2)
  static let top = Self(rawValue: 1 << 3)
}

private final class BarGestureHandle: NSView {
  var isGestureEnabled = true {
    didSet { alphaValue = isGestureEnabled ? 0.38 : 0.18 }
  }
  var minimumSize = CGSize(width: 80, height: 28)
  var movementBounds = CGRect.zero
  var onFrameChanged: ((CGRect) -> Void)?

  private let dots: [CALayer] = (0..<6).map { _ in CALayer() }
  private var trackingArea: NSTrackingArea?

  var dotCount: Int { dots.count }

  init(toolTip: String) {
    super.init(frame: .zero)
    wantsLayer = true
    layer?.cornerRadius = 4
    self.toolTip = toolTip
    for dot in dots {
      dot.cornerRadius = 1.25
      dot.backgroundColor = NSColor.secondaryLabelColor.cgColor
      layer?.addSublayer(dot)
    }
    setAccessibilityElement(true)
    setAccessibilityRole(.button)
    setAccessibilityLabel(toolTip)
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }

  override func layout() {
    super.layout()
    let dotSize: CGFloat = 2.5
    let horizontalGap: CGFloat = 3.5
    let verticalGap: CGFloat = 2.5
    let width = dotSize * 2 + horizontalGap
    let height = dotSize * 3 + verticalGap * 2
    let origin = CGPoint(x: (bounds.width - width) / 2, y: (bounds.height - height) / 2)
    for index in dots.indices {
      let column = CGFloat(index % 2)
      let row = CGFloat(index / 2)
      dots[index].frame = CGRect(
        x: origin.x + column * (dotSize + horizontalGap),
        y: origin.y + row * (dotSize + verticalGap),
        width: dotSize,
        height: dotSize
      )
      dots[index].cornerRadius = dotSize / 2
    }
  }

  override func updateTrackingAreas() {
    super.updateTrackingAreas()
    if let trackingArea { removeTrackingArea(trackingArea) }
    let next = NSTrackingArea(
      rect: .zero,
      options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect],
      owner: self
    )
    addTrackingArea(next)
    trackingArea = next
  }

  override func mouseEntered(with event: NSEvent) {
    guard isGestureEnabled else { return }
    alphaValue = 1
    layer?.backgroundColor = NSColor.labelColor.withAlphaComponent(0.10).cgColor
    setDotColor(.controlAccentColor)
  }

  override func mouseExited(with event: NSEvent) {
    alphaValue = isGestureEnabled ? 0.38 : 0.18
    layer?.backgroundColor = NSColor.clear.cgColor
    setDotColor(.secondaryLabelColor)
  }

  override func resetCursorRects() {
    guard isGestureEnabled else { return }
    addCursorRect(bounds, cursor: .openHand)
  }

  override func mouseDown(with event: NSEvent) {
    guard isGestureEnabled, let window else { return }
    let initialMouse = NSEvent.mouseLocation
    let initialFrame = window.frame
    NSCursor.closedHand.push()
    defer { NSCursor.pop() }

    while let next = window.nextEvent(matching: [.leftMouseDragged, .leftMouseUp]) {
      if next.type == .leftMouseUp { return }
      let currentMouse = NSEvent.mouseLocation
      let delta = CGPoint(
        x: currentMouse.x - initialMouse.x,
        y: currentMouse.y - initialMouse.y
      )
      let nextFrame = BarWindowGeometry.constrained(
        initialFrame.offsetBy(dx: delta.x, dy: delta.y),
        to: movementBounds,
        minimumSize: minimumSize
      )
      window.setFrame(nextFrame, display: true)
      onFrameChanged?(nextFrame)
    }
  }

  private func setDotColor(_ color: NSColor) {
    for dot in dots { dot.backgroundColor = color.cgColor }
  }
}

final class AdjustableWorkspaceBarView: NSView {
  static let leadingChromeWidth: CGFloat = 18
  private static let resizeEdgeWidth: CGFloat = 5

  private let content: WorkspaceBarView
  private let materialView = NSVisualEffectView()
  private let moveHandle = BarGestureHandle(
    toolTip: "Drag PaneruBar"
  )
  private var minimumContentSize: CGSize
  private var allowedFrame: CGRect
  private let surfaceCornerRadius: CGFloat
  private let onFrameChanged: (CGRect) -> Void
  private var resizeTrackingAreas: [NSTrackingArea] = []

  private(set) var isLocked: Bool
  var renderedGripDotCount: Int { moveHandle.dotCount }
  var isMoveHandleVisible: Bool { !moveHandle.isHidden }
  var ownerDisplayFrame: CGRect { allowedFrame }
  var renderedSurfaceMaterial: NSVisualEffectView.Material { materialView.material }
  var renderedSurfaceTintColor: CGColor? { materialView.layer?.backgroundColor }

  static func chromeWidth(isLocked: Bool) -> CGFloat {
    isLocked ? 0 : leadingChromeWidth
  }

  init(
    content: WorkspaceBarView,
    preferences: BarPreferences,
    minimumContentSize: CGSize,
    allowedFrame: CGRect,
    isLocked: Bool,
    onFrameChanged: @escaping (CGRect) -> Void
  ) {
    self.content = content
    self.minimumContentSize = minimumContentSize
    self.allowedFrame = allowedFrame
    self.surfaceCornerRadius = preferences.cornerRadius
    self.isLocked = isLocked
    self.onFrameChanged = onFrameChanged
    super.init(frame: .zero)
    applyAppearance(preferences)
    build()
    applyLockState()
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }

  override func layout() {
    super.layout()
    layer?.cornerRadius = min(surfaceCornerRadius, bounds.height / 2)
    materialView.frame = bounds
    materialView.layer?.cornerRadius = layer?.cornerRadius ?? 0
    let leadingWidth = Self.chromeWidth(isLocked: isLocked)
    moveHandle.frame = CGRect(x: 0, y: 0, width: leadingWidth, height: bounds.height)
    content.frame = CGRect(
      x: leadingWidth,
      y: 0,
      width: max(0, bounds.width - leadingWidth),
      height: bounds.height
    )
  }

  override func hitTest(_ point: NSPoint) -> NSView? {
    if !isLocked, !resizeEdges(at: point).isEmpty { return self }
    return super.hitTest(point)
  }

  override func updateTrackingAreas() {
    super.updateTrackingAreas()
    for area in resizeTrackingAreas { removeTrackingArea(area) }
    resizeTrackingAreas.removeAll()
    guard !isLocked else { return }
    for (rect, edges) in resizeRegions() {
      let area = NSTrackingArea(
        rect: rect,
        options: [.mouseEnteredAndExited, .mouseMoved, .activeAlways],
        owner: self,
        userInfo: ["edges": edges.rawValue]
      )
      addTrackingArea(area)
      resizeTrackingAreas.append(area)
    }
  }

  override func cursorUpdate(with event: NSEvent) {
    guard updateResizeCursor(with: event) else {
      super.cursorUpdate(with: event)
      return
    }
  }

  override func mouseEntered(with event: NSEvent) {
    _ = updateResizeCursor(with: event)
  }

  override func mouseMoved(with event: NSEvent) {
    _ = updateResizeCursor(with: event)
  }

  override func mouseExited(with event: NSEvent) {
    NSCursor.arrow.set()
    window?.invalidateCursorRects(for: self)
  }

  override func resetCursorRects() {
    guard !isLocked else { return }
    let edge = Self.resizeEdgeWidth
    addCursorRect(
      CGRect(x: 0, y: edge, width: edge, height: bounds.height - edge * 2), cursor: .resizeLeftRight
    )
    addCursorRect(
      CGRect(x: bounds.width - edge, y: edge, width: edge, height: bounds.height - edge * 2),
      cursor: .resizeLeftRight)
    addCursorRect(
      CGRect(x: edge, y: 0, width: bounds.width - edge * 2, height: edge), cursor: .resizeUpDown)
    addCursorRect(
      CGRect(x: edge, y: bounds.height - edge, width: bounds.width - edge * 2, height: edge),
      cursor: .resizeUpDown)
    addCursorRect(CGRect(x: 0, y: 0, width: edge, height: edge), cursor: Self.upLeftDownRightCursor)
    addCursorRect(
      CGRect(x: bounds.width - edge, y: bounds.height - edge, width: edge, height: edge),
      cursor: Self.upLeftDownRightCursor)
    addCursorRect(
      CGRect(x: bounds.width - edge, y: 0, width: edge, height: edge),
      cursor: Self.upRightDownLeftCursor)
    addCursorRect(
      CGRect(x: 0, y: bounds.height - edge, width: edge, height: edge),
      cursor: Self.upRightDownLeftCursor)
  }

  override func mouseDown(with event: NSEvent) {
    let edges = resizeEdges(at: convert(event.locationInWindow, from: nil))
    guard !isLocked, !edges.isEmpty, let window else {
      super.mouseDown(with: event)
      return
    }
    let initialMouse = NSEvent.mouseLocation
    let initialFrame = window.frame
    while let next = window.nextEvent(matching: [.leftMouseDragged, .leftMouseUp]) {
      if next.type == .leftMouseUp { return }
      let currentMouse = NSEvent.mouseLocation
      let delta = CGPoint(
        x: currentMouse.x - initialMouse.x,
        y: currentMouse.y - initialMouse.y
      )
      let nextFrame = BarWindowGeometry.resized(
        initialFrame,
        delta: delta,
        edges: edges,
        within: allowedFrame,
        minimumSize: panelMinimumSize
      )
      window.setFrame(nextFrame, display: true)
      onFrameChanged(nextFrame)
    }
  }

  func updateMinimumContentSize(_ size: CGSize) {
    minimumContentSize = size
    updateHandleMinimumSize()
  }

  func updateAllowedFrame(_ frame: CGRect) {
    allowedFrame = frame
    moveHandle.movementBounds = frame
  }

  func setLocked(_ locked: Bool) {
    guard locked != isLocked else { return }
    isLocked = locked
    applyLockState()
  }

  private func build() {
    addSubview(materialView)
    addSubview(moveHandle)
    addSubview(content)

    moveHandle.onFrameChanged = onFrameChanged
    moveHandle.movementBounds = allowedFrame
    menu = content.menu
    moveHandle.menu = content.menu
  }

  private func applyAppearance(_ preferences: BarPreferences) {
    wantsLayer = true
    layer?.backgroundColor = NSColor.clear.cgColor
    layer?.borderColor = (NSColor(paneruHex: preferences.borderColorHex) ?? .separatorColor).cgColor
    layer?.borderWidth = preferences.borderWidth
    layer?.cornerRadius = preferences.cornerRadius
    layer?.masksToBounds = true
    materialView.material = .menu
    materialView.blendingMode = .behindWindow
    materialView.state = .active
    materialView.isEmphasized = false
    materialView.wantsLayer = true
    materialView.layer?.backgroundColor =
      (NSColor(paneruHex: preferences.backgroundColorHex) ?? .windowBackgroundColor).cgColor
    materialView.layer?.masksToBounds = true
  }

  private func applyLockState() {
    moveHandle.isGestureEnabled = !isLocked
    moveHandle.isHidden = isLocked
    updateHandleMinimumSize()
    window?.invalidateCursorRects(for: self)
    if !bounds.isEmpty { updateTrackingAreas() }
    needsLayout = true
  }

  private func updateHandleMinimumSize() {
    moveHandle.minimumSize = panelMinimumSize
  }

  private var panelMinimumSize: CGSize {
    CGSize(
      width: minimumContentSize.width + Self.chromeWidth(isLocked: isLocked),
      height: minimumContentSize.height
    )
  }

  private func resizeEdges(at point: CGPoint) -> BarResizeEdges {
    let edge = Self.resizeEdgeWidth
    var result: BarResizeEdges = []
    if point.x <= edge { result.insert(.left) }
    if point.x >= bounds.width - edge { result.insert(.right) }
    if point.y <= edge { result.insert(.bottom) }
    if point.y >= bounds.height - edge { result.insert(.top) }
    return result
  }

  private func resizeRegions() -> [(CGRect, BarResizeEdges)] {
    let edge = Self.resizeEdgeWidth
    return [
      (CGRect(x: 0, y: edge, width: edge, height: bounds.height - edge * 2), [.left]),
      (
        CGRect(x: bounds.width - edge, y: edge, width: edge, height: bounds.height - edge * 2),
        [.right]
      ),
      (CGRect(x: edge, y: 0, width: bounds.width - edge * 2, height: edge), [.bottom]),
      (
        CGRect(x: edge, y: bounds.height - edge, width: bounds.width - edge * 2, height: edge),
        [.top]
      ),
      (CGRect(x: 0, y: 0, width: edge, height: edge), [.left, .bottom]),
      (CGRect(x: bounds.width - edge, y: 0, width: edge, height: edge), [.right, .bottom]),
      (CGRect(x: 0, y: bounds.height - edge, width: edge, height: edge), [.left, .top]),
      (
        CGRect(x: bounds.width - edge, y: bounds.height - edge, width: edge, height: edge),
        [.right, .top]
      ),
    ]
  }

  private func updateResizeCursor(with event: NSEvent) -> Bool {
    guard !isLocked else { return false }
    let trackedRawValue: UInt8? =
      switch event.type {
      case .mouseEntered, .mouseExited, .cursorUpdate:
        event.trackingArea?.userInfo?["edges"] as? UInt8
      default:
        nil
      }
    let edges =
      trackedRawValue.map(BarResizeEdges.init(rawValue:))
      ?? resizeEdges(at: convert(event.locationInWindow, from: nil))
    guard let cursor = resizeCursor(for: edges) else { return false }
    cursor.set()
    return true
  }

  private func resizeCursor(for edges: BarResizeEdges) -> NSCursor? {
    let horizontal = edges.contains(.left) || edges.contains(.right)
    let vertical = edges.contains(.top) || edges.contains(.bottom)
    if horizontal, vertical {
      let descending =
        (edges.contains(.left) && edges.contains(.top))
        || (edges.contains(.right) && edges.contains(.bottom))
      return descending ? Self.upLeftDownRightCursor : Self.upRightDownLeftCursor
    }
    if horizontal { return .resizeLeftRight }
    if vertical { return .resizeUpDown }
    return nil
  }

  private static let upLeftDownRightCursor = diagonalCursor(
    symbol: "arrow.up.left.and.arrow.down.right"
  )
  private static let upRightDownLeftCursor = diagonalCursor(
    symbol: "arrow.up.right.and.arrow.down.left"
  )

  private static func diagonalCursor(symbol: String) -> NSCursor {
    let size = CGSize(width: 18, height: 18)
    let image = NSImage(systemSymbolName: symbol, accessibilityDescription: "Resize") ?? NSImage()
    image.size = size
    return NSCursor(image: image, hotSpot: CGPoint(x: size.width / 2, y: size.height / 2))
  }
}
