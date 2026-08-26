import AppKit
import QuartzCore

extension NSPasteboard.PasteboardType {
  static let spoolWindowID = Self("com.wxxxcxx.spool-bar.window-id")
}

class BarActionButton: NSButton {
  var windowID: Int32?
  var workspaceNumber: UInt32?
  var iconKey: String?
  var windowFloating: Bool?
  var floatingBadge: CALayer?
  var onScroll: ((NSEvent) -> Void)?
  var motionSpec: BarMotionSpec?
  var allowsCommandInteraction = true
  private let selectionLayer = CALayer()
  private let selectionIndicatorLayer = CALayer()
  private let hoverLayer = CALayer()
  private var hoverTrackingArea: NSTrackingArea?

  override init(frame frameRect: NSRect) {
    super.init(frame: frameRect)
    wantsLayer = true
    selectionLayer.backgroundColor = NSColor.clear.cgColor
    selectionLayer.cornerRadius = 5
    layer?.insertSublayer(selectionLayer, at: 0)
    selectionIndicatorLayer.backgroundColor = NSColor.clear.cgColor
    layer?.insertSublayer(selectionIndicatorLayer, above: selectionLayer)
    hoverLayer.backgroundColor = NSColor.clear.cgColor
    hoverLayer.cornerRadius = 5
    layer?.insertSublayer(hoverLayer, above: selectionIndicatorLayer)
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }

  override var isHighlighted: Bool {
    didSet {
      guard allowsCommandInteraction, oldValue != isHighlighted else { return }
      BarMotion.setPressed(isHighlighted, on: layer, spec: motionSpec)
    }
  }

  override func layout() {
    super.layout()
    selectionLayer.frame = bounds
    let indicatorWidth = min(10, max(0, bounds.width - 6))
    selectionIndicatorLayer.frame = CGRect(
      x: bounds.midX - indicatorWidth / 2,
      y: 1,
      width: indicatorWidth,
      height: 2
    )
    selectionIndicatorLayer.cornerRadius = 1
    hoverLayer.frame = bounds
  }

  func setSelectedAppearance(fill: NSColor, accent: NSColor) {
    selectionLayer.backgroundColor = fill.cgColor
    selectionIndicatorLayer.backgroundColor = accent.cgColor
  }

  var renderedSelectionFillColor: CGColor? { selectionLayer.backgroundColor }
  var renderedSelectionIndicatorColor: CGColor? { selectionIndicatorLayer.backgroundColor }

  override func updateTrackingAreas() {
    super.updateTrackingAreas()
    if let hoverTrackingArea { removeTrackingArea(hoverTrackingArea) }
    let next = NSTrackingArea(
      rect: .zero,
      options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect],
      owner: self
    )
    addTrackingArea(next)
    hoverTrackingArea = next
  }

  override func resetCursorRects() {
    addCursorRect(bounds, cursor: allowsCommandInteraction ? .pointingHand : .arrow)
  }

  override func mouseEntered(with event: NSEvent) {
    guard allowsCommandInteraction else { return }
    BarMotion.setHoverColor(
      NSColor.labelColor.withAlphaComponent(0.10).cgColor,
      on: hoverLayer,
      spec: motionSpec
    )
  }

  override func mouseExited(with event: NSEvent) {
    BarMotion.setHoverColor(NSColor.clear.cgColor, on: hoverLayer, spec: motionSpec)
  }

  override func scrollWheel(with event: NSEvent) {
    guard let onScroll else {
      super.scrollWheel(with: event)
      return
    }
    onScroll(event)
  }
}

private final class WindowIconSurfaceView: NSView {
  let stackLayer = CALayer()
  private let imageLayer = CALayer()
  private var image: NSImage?

  override init(frame frameRect: NSRect) {
    super.init(frame: frameRect)
    wantsLayer = true
    layer?.masksToBounds = false
    stackLayer.anchorPoint = CGPoint(x: 0.5, y: 0.5)
    stackLayer.masksToBounds = false
    imageLayer.contentsGravity = .resizeAspect
    stackLayer.addSublayer(imageLayer)
    layer?.addSublayer(stackLayer)
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }

  override func layout() {
    super.layout()
    CATransaction.begin()
    CATransaction.setDisableActions(true)
    stackLayer.bounds = bounds
    stackLayer.position = CGPoint(x: bounds.midX, y: bounds.midY)
    imageLayer.frame = bounds.insetBy(dx: 2, dy: 2)
    CATransaction.commit()
  }

  override func hitTest(_ point: NSPoint) -> NSView? { nil }

  override func viewDidChangeBackingProperties() {
    super.viewDidChangeBackingProperties()
    updateImageContents()
  }

  func setImage(_ image: NSImage?) {
    self.image = image
    updateImageContents()
  }

  private func updateImageContents() {
    var proposedRect = NSRect(origin: .zero, size: image?.size ?? .zero)
    let contents = image?.cgImage(
      forProposedRect: &proposedRect,
      context: nil,
      hints: nil
    )
    CATransaction.begin()
    CATransaction.setDisableActions(true)
    imageLayer.contents = contents
    imageLayer.contentsScale = window?.backingScaleFactor ?? 2
    CATransaction.commit()
  }
}

final class WindowActionButton: BarActionButton, NSDraggingSource {
  private let iconSurface = WindowIconSurfaceView()
  var stackSurface: NSView { iconSurface }
  var stackLayer: CALayer { iconSurface.stackLayer }

  override init(frame frameRect: NSRect) {
    super.init(frame: frameRect)
    title = ""
    image = nil
    imagePosition = .noImage
    addSubview(iconSurface)
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }

  override func layout() {
    super.layout()
    iconSurface.frame = bounds
  }

  func setIcon(_ image: NSImage?) {
    iconSurface.setImage(image)
  }

  override func mouseDown(with event: NSEvent) {
    guard windowID != nil, let window else {
      super.mouseDown(with: event)
      return
    }

    let initialLocation = event.locationInWindow
    isHighlighted = true
    while let next = window.nextEvent(matching: [.leftMouseDragged, .leftMouseUp]) {
      switch next.type {
      case .leftMouseDragged:
        let offsetX = next.locationInWindow.x - initialLocation.x
        let offsetY = next.locationInWindow.y - initialLocation.y
        guard hypot(offsetX, offsetY) >= 3 else { continue }
        isHighlighted = false
        beginWindowDrag(with: next)
        return
      case .leftMouseUp:
        isHighlighted = false
        let location = convert(next.locationInWindow, from: nil)
        if bounds.contains(location) {
          _ = sendAction(action, to: target)
        }
        return
      default:
        continue
      }
    }
    isHighlighted = false
  }

  func draggingSession(
    _ session: NSDraggingSession,
    sourceOperationMaskFor context: NSDraggingContext
  ) -> NSDragOperation {
    .move
  }

  private func beginWindowDrag(with event: NSEvent) {
    guard let windowID else { return }
    let item = NSPasteboardItem()
    item.setString(String(windowID), forType: .spoolWindowID)
    let draggingItem = NSDraggingItem(pasteboardWriter: item)
    draggingItem.setDraggingFrame(bounds, contents: draggingImage())
    beginDraggingSession(with: [draggingItem], event: event, source: self)
  }

  private func draggingImage() -> NSImage {
    guard let representation = bitmapImageRepForCachingDisplay(in: bounds) else {
      return NSImage(size: bounds.size)
    }
    cacheDisplay(in: bounds, to: representation)
    let image = NSImage(size: bounds.size)
    image.addRepresentation(representation)
    return image
  }
}

final class FocusRingView: NSView {
  private let fillLayer = CALayer()
  private let indicatorLayer = CALayer()
  private var indicatorHeight: CGFloat = 1

  override init(frame frameRect: NSRect) {
    super.init(frame: frameRect)
    wantsLayer = true
    layer?.masksToBounds = false
    fillLayer.cornerRadius = 5
    layer?.addSublayer(fillLayer)
    layer?.addSublayer(indicatorLayer)
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }

  override func layout() {
    super.layout()
    fillLayer.frame = bounds
    let indicatorWidth = min(12, max(0, bounds.width - 6))
    indicatorLayer.frame = CGRect(
      x: bounds.midX - indicatorWidth / 2,
      y: 1,
      width: indicatorWidth,
      height: indicatorHeight
    )
    indicatorLayer.cornerRadius = indicatorHeight / 2
  }

  func apply(color: NSColor, indicatorHeight: CGFloat) {
    fillLayer.backgroundColor = color.withAlphaComponent(0.16).cgColor
    indicatorLayer.backgroundColor = color.cgColor
    self.indicatorHeight = max(1, indicatorHeight)
    needsLayout = true
  }

  var renderedFillColor: CGColor? { fillLayer.backgroundColor }
  var renderedIndicatorFrame: CGRect { indicatorLayer.frame }

  override func hitTest(_ point: NSPoint) -> NSView? { nil }
}

final class WorkspaceViewportView: NSView {
  override init(frame frameRect: NSRect) {
    super.init(frame: frameRect)
    wantsLayer = true
    layer?.masksToBounds = true
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }
}

final class WindowViewportView: NSView {
  var onScroll: ((NSEvent) -> Void)?
  private let motionView = NSView()

  override init(frame frameRect: NSRect) {
    super.init(frame: frameRect)
    wantsLayer = true
    layer?.masksToBounds = true
    motionView.wantsLayer = true
    addSubview(motionView)
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }

  override func layout() {
    super.layout()
    motionView.frame = bounds
  }

  var motionLayer: CALayer? { motionView.layer }
  var motionContentIsClipped: Bool {
    layer?.masksToBounds == true
      && motionView.superview === self
      && motionView.layer !== layer
  }

  func addContentSubview(_ view: NSView) {
    motionView.addSubview(view)
  }

  override func scrollWheel(with event: NSEvent) {
    onScroll?(event)
  }
}

final class BarSeparatorView: NSView {
  override init(frame frameRect: NSRect) {
    super.init(frame: frameRect)
    wantsLayer = true
    layer?.backgroundColor = NSColor.separatorColor.withAlphaComponent(0.55).cgColor
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }

  override func hitTest(_ point: NSPoint) -> NSView? { nil }
}

enum WorkspaceCycle {
  static func target(
    numbers: [UInt32],
    current: UInt32,
    step: Int
  ) -> UInt32? {
    guard numbers.count > 1,
      step != 0,
      let index = numbers.firstIndex(of: current)
    else { return nil }
    let targetIndex = index + (step > 0 ? 1 : -1)
    guard numbers.indices.contains(targetIndex) else { return nil }
    return numbers[targetIndex]
  }
}

final class AppIconProvider {
  private var cache: [String: NSImage] = [:]

  func icon(bundleID: String, appName: String) -> NSImage {
    let key = bundleID.isEmpty ? appName : bundleID
    if let cached = cache[key] { return cached.copy() as! NSImage }

    let image: NSImage
    if !bundleID.isEmpty,
      let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID)
    {
      image = NSWorkspace.shared.icon(forFile: url.path)
    } else {
      image = NSImage(systemSymbolName: "app", accessibilityDescription: appName) ?? NSImage()
    }
    cache[key] = image
    return image.copy() as! NSImage
  }
}
