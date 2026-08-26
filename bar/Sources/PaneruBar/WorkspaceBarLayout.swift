import CoreGraphics

struct WorkspaceBarMetrics: Equatable {
  let iconButtonSize: CGFloat
  let navigationButtonWidth: CGFloat
  let horizontalPadding: CGFloat
  let workspaceSpacing: CGFloat
  let windowSpacing: CGFloat

  var windowStride: CGFloat { iconButtonSize + windowSpacing }

  func windowWidth(count: Int) -> CGFloat {
    guard count > 0 else { return 0 }
    return CGFloat(count) * iconButtonSize + CGFloat(count - 1) * windowSpacing
  }
}

struct WorkspaceBarLayoutInput: Equatable {
  let bounds: CGRect
  let workspaceButtonWidth: CGFloat
  let windowCount: Int
  let windowOffset: Int
  let metrics: WorkspaceBarMetrics
}

struct WorkspaceBarLayoutResult: Equatable {
  let previousWorkspaceFrame: CGRect
  let workspaceFrame: CGRect
  let nextWorkspaceFrame: CGRect
  let separatorFrame: CGRect?
  let previousWindowFrame: CGRect
  let windowViewportFrame: CGRect
  let nextWindowFrame: CGRect
  let windowCapacity: Int
  let windowOffset: Int
  let visibleWindowRange: Range<Int>
  let windowCount: Int
  let showsWindowOverflow: Bool

  var canPageWindowsBackward: Bool {
    showsWindowOverflow && windowOffset > 0
  }

  var canPageWindowsForward: Bool {
    showsWindowOverflow && visibleWindowRange.upperBound < windowCount
  }
}

enum WorkspaceBarLayout {
  private static let separatorWidth: CGFloat = 1

  static func resolve(_ input: WorkspaceBarLayoutInput) -> WorkspaceBarLayoutResult {
    let metrics = input.metrics
    let side = metrics.iconButtonSize
    let y = input.bounds.midY - side / 2
    let previousWorkspaceFrame = CGRect(
      x: metrics.horizontalPadding,
      y: y,
      width: metrics.navigationButtonWidth,
      height: side
    )
    let workspaceFrame = CGRect(
      x: previousWorkspaceFrame.maxX,
      y: y,
      width: input.workspaceButtonWidth,
      height: side
    )
    let nextWorkspaceFrame = CGRect(
      x: workspaceFrame.maxX,
      y: y,
      width: metrics.navigationButtonWidth,
      height: side
    )

    guard input.windowCount > 0 else {
      return WorkspaceBarLayoutResult(
        previousWorkspaceFrame: previousWorkspaceFrame,
        workspaceFrame: workspaceFrame,
        nextWorkspaceFrame: nextWorkspaceFrame,
        separatorFrame: nil,
        previousWindowFrame: .zero,
        windowViewportFrame: .zero,
        nextWindowFrame: .zero,
        windowCapacity: 0,
        windowOffset: 0,
        visibleWindowRange: 0..<0,
        windowCount: 0,
        showsWindowOverflow: false
      )
    }

    let gap = max(4, metrics.workspaceSpacing)
    let separatorHeight = min(12, max(0, side - 8))
    let separatorFrame = CGRect(
      x: nextWorkspaceFrame.maxX + floor((gap - Self.separatorWidth) / 2),
      y: input.bounds.midY - separatorHeight / 2,
      width: Self.separatorWidth,
      height: separatorHeight
    )
    let windowX = nextWorkspaceFrame.maxX + gap
    let availableWidth = max(
      0,
      input.bounds.maxX - metrics.horizontalPadding - windowX
    )
    let showsOverflow =
      input.windowCount > 1
      && metrics.windowWidth(count: input.windowCount) > availableWidth

    let previousWindowFrame: CGRect
    let nextWindowFrame: CGRect
    let viewportFrame: CGRect
    if showsOverflow {
      previousWindowFrame = CGRect(
        x: windowX,
        y: y,
        width: metrics.navigationButtonWidth,
        height: side
      )
      nextWindowFrame = CGRect(
        x: input.bounds.maxX - metrics.horizontalPadding - metrics.navigationButtonWidth,
        y: y,
        width: metrics.navigationButtonWidth,
        height: side
      )
      viewportFrame = CGRect(
        x: previousWindowFrame.maxX,
        y: y,
        width: max(0, nextWindowFrame.minX - previousWindowFrame.maxX),
        height: side
      )
    } else {
      previousWindowFrame = .zero
      nextWindowFrame = .zero
      viewportFrame = CGRect(x: windowX, y: y, width: availableWidth, height: side)
    }

    let capacity = max(
      0,
      Int(floor((viewportFrame.width + metrics.windowSpacing) / metrics.windowStride))
    )
    let offset = min(max(0, input.windowOffset), max(0, input.windowCount - capacity))
    let visibleCount = min(capacity, max(0, input.windowCount - offset))
    let range = offset..<(offset + visibleCount)

    return WorkspaceBarLayoutResult(
      previousWorkspaceFrame: previousWorkspaceFrame,
      workspaceFrame: workspaceFrame,
      nextWorkspaceFrame: nextWorkspaceFrame,
      separatorFrame: separatorFrame,
      previousWindowFrame: previousWindowFrame,
      windowViewportFrame: viewportFrame,
      nextWindowFrame: nextWindowFrame,
      windowCapacity: capacity,
      windowOffset: offset,
      visibleWindowRange: range,
      windowCount: input.windowCount,
      showsWindowOverflow: showsOverflow
    )
  }

  static func estimatedWidth(
    workspaceButtonWidth: CGFloat,
    windowCount: Int,
    metrics: WorkspaceBarMetrics
  ) -> CGFloat {
    let workspaceWidth = metrics.navigationButtonWidth * 2 + workspaceButtonWidth
    let windowsWidth = metrics.windowWidth(count: windowCount)
    let gap = windowCount == 0 ? 0 : max(4, metrics.workspaceSpacing)
    return metrics.horizontalPadding * 2 + workspaceWidth + gap + windowsWidth
  }

  static func minimumWidth(
    workspaceButtonWidth: CGFloat,
    windowCount: Int,
    metrics: WorkspaceBarMetrics
  ) -> CGFloat {
    let workspaceWidth = metrics.navigationButtonWidth * 2 + workspaceButtonWidth
    let windowsWidth: CGFloat
    if windowCount > 1 {
      windowsWidth = metrics.iconButtonSize + metrics.navigationButtonWidth * 2
    } else {
      windowsWidth = windowCount == 0 ? 0 : metrics.iconButtonSize
    }
    let gap = windowCount == 0 ? 0 : max(4, metrics.workspaceSpacing)
    return metrics.horizontalPadding * 2 + workspaceWidth + gap + windowsWidth
  }
}
