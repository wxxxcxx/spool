import AppKit
import QuartzCore

final class WorkspacePanel: NSPanel {
  override var canBecomeKey: Bool { false }
  override var canBecomeMain: Bool { false }

  override func constrainFrameRect(_ frameRect: NSRect, to screen: NSScreen?) -> NSRect {
    frameRect
  }
}

final class WorkspaceBarController {
  private struct PanelKey: Hashable {
    let displayID: UInt32
    let slot: Int
  }

  private struct PanelRecord {
    let panel: WorkspacePanel
    let view: WorkspaceBarView
    let container: AdjustableWorkspaceBarView
  }

  private let client: SpoolClient
  private let configurationPanel: ConfigurationPanelController
  private var preferences: BarPreferences
  private let iconProvider = AppIconProvider()
  private let windowStateStore = BarWindowStateStore()
  private var state: SpoolStateDocument?
  private var panels: [PanelKey: PanelRecord] = [:]
  private var screenSignature = ""
  private var geometryTimer: Timer?
  private var settingsAnchorKey: PanelKey?

  init(
    client: SpoolClient,
    preferences: BarPreferences = BarPreferences(),
    savePreferences: @escaping (BarPreferences) throws -> Void = { _ in }
  ) {
    self.client = client
    self.preferences = preferences
    configurationPanel = ConfigurationPanelController(
      preferences: preferences,
      save: savePreferences
    )
  }

  func start() {
    NotificationCenter.default.addObserver(
      self,
      selector: #selector(screenParametersChanged),
      name: NSApplication.didChangeScreenParametersNotification,
      object: nil
    )
    NSWorkspace.shared.notificationCenter.addObserver(
      self,
      selector: #selector(screenParametersChanged),
      name: NSWorkspace.activeSpaceDidChangeNotification,
      object: nil
    )
    geometryTimer = Timer.scheduledTimer(
      timeInterval: 1,
      target: self,
      selector: #selector(checkScreenGeometry),
      userInfo: nil,
      repeats: true
    )
    rebuild()
  }

  func update(_ state: SpoolStateDocument) {
    self.state = state
    rebuild()
  }

  func apply(_ preferences: BarPreferences) {
    guard preferences != self.preferences else { return }
    self.preferences = preferences
    configurationPanel.apply(preferences)
    closePanels()
    rebuild()
  }

  func stop() {
    geometryTimer?.invalidate()
    geometryTimer = nil
    NotificationCenter.default.removeObserver(self)
    NSWorkspace.shared.notificationCenter.removeObserver(self)
    configurationPanel.close()
    closePanels()
  }

  @objc private func screenParametersChanged() { rebuild() }

  @objc private func checkScreenGeometry() {
    let next = currentScreenSignature()
    if next != screenSignature { rebuild() }
  }

  private func rebuild() {
    precondition(Thread.isMainThread)
    screenSignature = currentScreenSignature()
    guard let state else {
      closePanels()
      return
    }
    configurationPanel.updateWorkspaceNumbers(
      Array(Set(state.virtualWorkspaces.map(\.number))).sorted()
    )

    var retained = Set<PanelKey>()

    for screen in NSScreen.screens {
      guard let displayID = screen.displayID,
        let display = state.display(displayID)
      else { continue }
      let visibleWorkspaces = state.visibleWorkspaces(on: display)
      guard let activeWorkspace = visibleWorkspaces.first(where: \.selected) else { continue }
      let geometry = ScreenGeometry(
        frame: screen.frame,
        visibleFrame: screen.visibleFrame,
        safeTopInset: screen.safeAreaInsets.top,
        auxiliaryLeft: screen.auxiliaryTopLeftArea,
        auxiliaryRight: screen.auxiliaryTopRightArea,
        trayFrames: MenuBarOccupancy.trayFrames(on: screen)
      )
      let windowState = windowStateStore.state(for: displayID)
      let chromeWidth = AdjustableWorkspaceBarView.chromeWidth(
        isLocked: windowState.isLocked
      )
      let minimumContentWidth = WorkspaceBarView.minimumWidth(
        workspace: activeWorkspace,
        preferences: preferences
      )
      let plan = SafeAreaPlanner.plan(
        contentWidth: WorkspaceBarView.estimatedWidth(
          workspace: activeWorkspace,
          preferences: preferences
        ) + chromeWidth,
        minimumWidth: minimumContentWidth + chromeWidth,
        geometry: geometry,
        barHeight: preferences.barHeight
      )
      let minimumSize = CGSize(
        width: minimumContentWidth,
        height: preferences.barHeight
      )
      let targetFrame: CGRect
      if let storedFrame = windowState.frame {
        targetFrame = BarWindowGeometry.constrained(
          storedFrame,
          to: screen.frame,
          minimumSize: CGSize(
            width: minimumSize.width + chromeWidth,
            height: minimumSize.height
          )
        )
      } else if let plan {
        targetFrame = plan.frame
      } else {
        continue
      }

      let key = PanelKey(displayID: displayID, slot: 0)
      retained.insert(key)
      if let existing = panels[key] {
        existing.container.updateMinimumContentSize(minimumSize)
        existing.container.updateAllowedFrame(screen.frame)
        existing.container.setLocked(windowState.isLocked)
        if existing.panel.frame != targetFrame {
          animate(panel: existing.panel, to: targetFrame)
        }
        existing.view.update(workspaces: visibleWorkspaces)
        continue
      }

      let view = WorkspaceBarView(
        displayID: displayID,
        workspaces: visibleWorkspaces,
        preferences: preferences,
        client: client,
        iconProvider: iconProvider,
        onSettingsRequested: { [weak self] anchor, screen in
          self?.toggleSettings(for: key, anchor: anchor, screen: screen)
        },
        isBarLocked: { [weak self] in
          self?.windowStateStore.state(for: displayID).isLocked ?? true
        },
        onBarLockChanged: { [weak self] locked in
          guard let self else { return }
          self.panels[key]?.container.setLocked(locked)
          var next = self.windowStateStore.state(for: displayID)
          next.frame = self.panels[key]?.panel.frame
          next.isLocked = locked
          self.windowStateStore.save(next, for: displayID)
        }
      )
      let container = AdjustableWorkspaceBarView(
        content: view,
        preferences: preferences,
        minimumContentSize: minimumSize,
        allowedFrame: screen.frame,
        isLocked: windowState.isLocked,
        onFrameChanged: { [weak self] frame in
          guard let self else { return }
          var next = self.windowStateStore.state(for: displayID)
          next.frame = frame
          self.windowStateStore.save(next, for: displayID)
          self.repositionConfigurationPanel()
        }
      )
      let panel = makePanel(frame: targetFrame, content: container)
      panels[key] = PanelRecord(panel: panel, view: view, container: container)
      panel.orderFrontRegardless()
    }

    let staleKeys = panels.keys.filter { !retained.contains($0) }
    for key in staleKeys {
      panels.removeValue(forKey: key)?.panel.close()
    }
    repositionConfigurationPanel()
  }

  private func animate(panel: NSPanel, to frame: CGRect) {
    guard
      let motion = BarMotionSpec.resolve(
        style: preferences.animationStyle,
        duration: preferences.animationDuration,
        reduceMotion: NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
      )
    else {
      panel.setFrame(frame, display: true)
      repositionConfigurationPanel()
      return
    }
    NSAnimationContext.runAnimationGroup { context in
      context.duration = motion.duration
      context.timingFunction = motion.timingFunction
      panel.animator().setFrame(frame, display: true)
    } completionHandler: { [weak self] in
      self?.repositionConfigurationPanel()
    }
  }

  private func toggleSettings(for key: PanelKey, anchor: CGRect, screen: NSScreen?) {
    settingsAnchorKey = key
    configurationPanel.toggle(anchor: anchor, screen: screen)
  }

  private func repositionConfigurationPanel() {
    guard configurationPanel.isVisible else { return }
    guard let key = settingsAnchorKey,
      let record = panels[key],
      let anchor = record.view.settingsButtonScreenRect
    else {
      configurationPanel.close()
      settingsAnchorKey = nil
      return
    }
    configurationPanel.reposition(anchor: anchor, screen: record.panel.screen)
  }

  private func makePanel(frame: CGRect, content: NSView) -> WorkspacePanel {
    let panel = WorkspacePanel(
      contentRect: frame,
      styleMask: [.borderless, .nonactivatingPanel],
      backing: .buffered,
      defer: false
    )
    panel.isOpaque = false
    panel.backgroundColor = .clear
    panel.hasShadow = preferences.showsShadow
    panel.hidesOnDeactivate = false
    panel.isFloatingPanel = true
    panel.level = NSWindow.Level(rawValue: NSWindow.Level.statusBar.rawValue + 1)
    panel.becomesKeyOnlyIfNeeded = true
    panel.acceptsMouseMovedEvents = true
    panel.collectionBehavior = [
      .canJoinAllSpaces,
      .transient,
      .fullScreenAuxiliary,
      .ignoresCycle,
    ]
    content.frame = CGRect(origin: .zero, size: frame.size)
    content.autoresizingMask = [.width, .height]
    panel.contentView = content
    return panel
  }

  private func closePanels() {
    for record in panels.values { record.panel.close() }
    panels.removeAll()
  }

  private func currentScreenSignature() -> String {
    NSScreen.screens.map { screen in
      let id = screen.displayID ?? 0
      let left = screen.auxiliaryTopLeftArea ?? .zero
      let right = screen.auxiliaryTopRightArea ?? .zero
      let tray = MenuBarOccupancy.trayFrames(on: screen)
        .map { "\($0.minX):\($0.width)" }
        .sorted()
        .joined(separator: ",")
      return
        "\(id):\(screen.frame):\(screen.visibleFrame):\(screen.safeAreaInsets):\(left):\(right):\(tray)"
    }.joined(separator: "|")
  }
}
