import AppKit

struct ConfigurationBubblePlacement: Equatable {
  let frame: CGRect
  let arrowX: CGFloat

  static func place(
    below anchor: CGRect,
    in visibleFrame: CGRect,
    panelSize: CGSize,
    gap: CGFloat = 4
  ) -> ConfigurationBubblePlacement {
    let margin: CGFloat = 8
    let idealX = anchor.midX - panelSize.width / 2
    let minimumX = visibleFrame.minX + margin
    let maximumX = max(minimumX, visibleFrame.maxX - panelSize.width - margin)
    let x = min(max(idealX, minimumX), maximumX)
    let idealY = anchor.minY - panelSize.height - gap
    let y = max(visibleFrame.minY + margin, idealY)
    let arrowX = min(max(anchor.midX - x, 18), panelSize.width - 18)
    return ConfigurationBubblePlacement(
      frame: CGRect(origin: CGPoint(x: x, y: y), size: panelSize),
      arrowX: arrowX
    )
  }
}

final class ConfigurationPanelController {
  static let panelSize = CGSize(width: 380, height: 520)
  static let panelStyleMask: NSWindow.StyleMask = [.borderless]

  private var preferences: BarPreferences
  private var workspaceNumbers: [UInt32] = []
  private let save: (BarPreferences) throws -> Void
  private lazy var bubbleView = ConfigurationBubbleView(
    preferences: preferences,
    workspaceNumbers: workspaceNumbers,
    onChange: save,
    onClose: { [weak self] in self?.close() }
  )
  private lazy var panel = makePanel(content: bubbleView)

  init(
    preferences: BarPreferences,
    save: @escaping (BarPreferences) throws -> Void
  ) {
    self.preferences = preferences
    self.save = save
  }

  var isVisible: Bool { panel.isVisible }

  func toggle(anchor: CGRect, screen: NSScreen?) {
    if panel.isVisible {
      close()
      return
    }
    bubbleView.apply(preferences, workspaceNumbers: workspaceNumbers)
    reposition(anchor: anchor, screen: screen)
    NSApplication.shared.activate(ignoringOtherApps: true)
    panel.makeKeyAndOrderFront(nil)
  }

  func apply(_ preferences: BarPreferences) {
    self.preferences = preferences
    bubbleView.apply(preferences, workspaceNumbers: workspaceNumbers)
  }

  func updateWorkspaceNumbers(_ workspaceNumbers: [UInt32]) {
    self.workspaceNumbers = workspaceNumbers
    bubbleView.apply(preferences, workspaceNumbers: workspaceNumbers)
  }

  func reposition(anchor: CGRect, screen: NSScreen?) {
    guard let visibleFrame = screen?.visibleFrame ?? NSScreen.main?.visibleFrame else { return }
    let placement = ConfigurationBubblePlacement.place(
      below: anchor,
      in: visibleFrame,
      panelSize: Self.panelSize
    )
    bubbleView.arrowX = placement.arrowX
    panel.setFrame(placement.frame, display: true)
  }

  func close() {
    panel.orderOut(nil)
  }

  private func makePanel(content: NSView) -> ConfigurationPanelWindow {
    let panel = ConfigurationPanelWindow(
      contentRect: CGRect(origin: .zero, size: Self.panelSize),
      styleMask: Self.panelStyleMask,
      backing: .buffered,
      defer: false
    )
    panel.isOpaque = false
    panel.backgroundColor = .clear
    panel.hasShadow = true
    panel.hidesOnDeactivate = false
    panel.isFloatingPanel = true
    panel.level = NSWindow.Level(rawValue: NSWindow.Level.statusBar.rawValue + 2)
    panel.becomesKeyOnlyIfNeeded = false
    panel.collectionBehavior = [
      .canJoinAllSpaces,
      .transient,
      .fullScreenAuxiliary,
      .ignoresCycle,
    ]
    content.frame = CGRect(origin: .zero, size: Self.panelSize)
    content.autoresizingMask = [.width, .height]
    panel.contentView = content
    return panel
  }
}

private final class ConfigurationPanelWindow: NSPanel {
  override var canBecomeKey: Bool { true }
  override var canBecomeMain: Bool { false }

  override func cancelOperation(_ sender: Any?) {
    orderOut(nil)
  }
}

private final class ConfigurationBubbleView: NSView {
  private static let arrowHeight: CGFloat = 10
  private let body = NSVisualEffectView()
  private let arrow = BubbleArrowView()
  private let form: ConfigurationFormView

  var arrowX: CGFloat = ConfigurationPanelController.panelSize.width / 2 {
    didSet { needsLayout = true }
  }

  init(
    preferences: BarPreferences,
    workspaceNumbers: [UInt32],
    onChange: @escaping (BarPreferences) throws -> Void,
    onClose: @escaping () -> Void
  ) {
    form = ConfigurationFormView(
      preferences: preferences,
      workspaceNumbers: workspaceNumbers,
      onChange: onChange,
      onClose: onClose
    )
    super.init(frame: .zero)
    wantsLayer = true

    body.material = .popover
    body.blendingMode = .behindWindow
    body.state = .active
    body.wantsLayer = true
    body.layer?.cornerRadius = 8
    body.layer?.borderWidth = 0.5
    body.layer?.borderColor = NSColor.separatorColor.cgColor
    body.layer?.masksToBounds = true
    addSubview(body)
    addSubview(arrow)

    form.translatesAutoresizingMaskIntoConstraints = false
    body.addSubview(form)
    NSLayoutConstraint.activate([
      form.leadingAnchor.constraint(equalTo: body.leadingAnchor, constant: 14),
      form.trailingAnchor.constraint(equalTo: body.trailingAnchor, constant: -14),
      form.topAnchor.constraint(equalTo: body.topAnchor, constant: 12),
      form.bottomAnchor.constraint(equalTo: body.bottomAnchor, constant: -12),
    ])
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }

  override func layout() {
    super.layout()
    body.frame = CGRect(x: 0, y: 0, width: bounds.width, height: bounds.height - Self.arrowHeight)
    arrow.frame = CGRect(
      x: arrowX - 11,
      y: body.frame.maxY - 1,
      width: 22,
      height: Self.arrowHeight + 1
    )
  }

  func apply(_ preferences: BarPreferences, workspaceNumbers: [UInt32]) {
    form.apply(preferences, workspaceNumbers: workspaceNumbers)
  }
}

private final class BubbleArrowView: NSView {
  override var isOpaque: Bool { false }

  override func draw(_ dirtyRect: NSRect) {
    super.draw(dirtyRect)
    let path = NSBezierPath()
    path.move(to: CGPoint(x: 1, y: 1))
    path.line(to: CGPoint(x: bounds.midX, y: bounds.maxY - 1))
    path.line(to: CGPoint(x: bounds.maxX - 1, y: 1))
    path.close()
    NSColor.windowBackgroundColor.setFill()
    path.fill()
    NSColor.separatorColor.setStroke()
    path.lineWidth = 0.5
    path.stroke()
  }
}

private final class ConfigurationFormView: NSView {
  private var preferences: BarPreferences
  private let onChange: (BarPreferences) throws -> Void
  private let onClose: () -> Void
  private var applying = false

  private let errorLabel = NSTextField(labelWithString: "")
  private let heightSlider = ConfigurationFormView.slider(22...44)
  private let heightValue = ConfigurationFormView.valueLabel()
  private let verticalPaddingSlider = ConfigurationFormView.slider(0...8)
  private let verticalPaddingValue = ConfigurationFormView.valueLabel()
  private let workspaceSpacingSlider = ConfigurationFormView.slider(2...14)
  private let workspaceSpacingValue = ConfigurationFormView.valueLabel()
  private let windowSpacingSlider = ConfigurationFormView.slider(0...14)
  private let windowSpacingValue = ConfigurationFormView.valueLabel()
  private let horizontalPaddingSlider = ConfigurationFormView.slider(4...20)
  private let horizontalPaddingValue = ConfigurationFormView.valueLabel()
  private let backgroundColorWell = NSColorWell()
  private let borderColorWell = NSColorWell()
  private let borderWidthSlider = ConfigurationFormView.slider(0...4)
  private let borderWidthValue = ConfigurationFormView.valueLabel()
  private let cornerRadiusSlider = ConfigurationFormView.slider(0...16)
  private let cornerRadiusValue = ConfigurationFormView.valueLabel()
  private let shadowButton = NSButton(
    checkboxWithTitle: "Show window shadow",
    target: nil,
    action: nil
  )
  private let focusColorWell = NSColorWell()
  private let activeWorkspaceColorWell = NSColorWell()
  private let floatingStylePopup = NSPopUpButton()
  private let focusRingButton = NSButton(
    checkboxWithTitle: "Show focus indicator", target: nil, action: nil)
  private let focusRingWidthSlider = ConfigurationFormView.slider(0.5...4)
  private let focusRingWidthValue = ConfigurationFormView.valueLabel()

  private let workspaceLabelsStack = NSStackView()
  private var workspaceNumbers: [UInt32]
  private var workspaceLabelFields: [UInt32: NSTextField] = [:]

  private let animationControl = NSSegmentedControl(
    labels: ["None", "Smooth", "Spring"],
    trackingMode: .selectOne,
    target: nil,
    action: nil
  )
  private let durationSlider = ConfigurationFormView.slider(0.08...0.6)
  private let durationValue = ConfigurationFormView.valueLabel()

  init(
    preferences: BarPreferences,
    workspaceNumbers: [UInt32],
    onChange: @escaping (BarPreferences) throws -> Void,
    onClose: @escaping () -> Void
  ) {
    self.preferences = preferences
    self.workspaceNumbers = workspaceNumbers
    self.onChange = onChange
    self.onClose = onClose
    super.init(frame: .zero)
    build()
    apply(preferences, workspaceNumbers: workspaceNumbers)
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }

  func apply(_ preferences: BarPreferences, workspaceNumbers: [UInt32]? = nil) {
    self.preferences = preferences
    if let workspaceNumbers {
      self.workspaceNumbers = workspaceNumbers
    }
    applying = true
    heightSlider.doubleValue = Double(preferences.barHeight)
    verticalPaddingSlider.maxValue = Double(preferences.maximumVerticalPadding)
    verticalPaddingSlider.numberOfTickMarks = Int(verticalPaddingSlider.maxValue) + 1
    verticalPaddingSlider.doubleValue = Double(preferences.effectiveVerticalPadding)
    workspaceSpacingSlider.doubleValue = Double(preferences.itemSpacing)
    windowSpacingSlider.doubleValue = Double(preferences.windowSpacing)
    horizontalPaddingSlider.doubleValue = Double(preferences.horizontalPadding)
    backgroundColorWell.color =
      NSColor(
        paneruHex: preferences.backgroundColorHex
      ) ?? .windowBackgroundColor
    borderColorWell.color =
      NSColor(
        paneruHex: preferences.borderColorHex
      ) ?? .separatorColor
    borderWidthSlider.doubleValue = Double(preferences.borderWidth)
    cornerRadiusSlider.doubleValue = Double(preferences.cornerRadius)
    shadowButton.state = preferences.showsShadow ? .on : .off
    focusColorWell.color =
      NSColor(
        paneruHex: preferences.selectionColorHex
      ) ?? .controlAccentColor
    activeWorkspaceColorWell.color =
      NSColor(
        paneruHex: preferences.activeWorkspaceColorHex
      ) ?? .controlAccentColor
    if let item = floatingStylePopup.itemArray.first(where: {
      $0.representedObject as? String == preferences.floatingIconStyle.rawValue
    }) {
      floatingStylePopup.select(item)
    }
    focusRingButton.state = preferences.showsFocusRing ? .on : .off
    focusRingWidthSlider.doubleValue = Double(preferences.focusRingWidth)
    animationControl.selectedSegment =
      switch preferences.animationStyle {
      case .none: 0
      case .smooth: 1
      case .spring: 2
      }
    durationSlider.doubleValue = preferences.animationDuration
    rebuildWorkspaceLabelFields()
    updateValueLabels()
    updateEnabledStates()
    applying = false
  }

  private func build() {
    let header = NSStackView()
    header.orientation = .horizontal
    header.alignment = .centerY
    header.spacing = 8

    let icon = NSImageView()
    icon.image = NSImage(systemSymbolName: "gearshape", accessibilityDescription: "Settings")
    icon.contentTintColor = .secondaryLabelColor
    icon.translatesAutoresizingMaskIntoConstraints = false
    icon.widthAnchor.constraint(equalToConstant: 18).isActive = true
    icon.heightAnchor.constraint(equalToConstant: 18).isActive = true
    header.addArrangedSubview(icon)

    let title = NSTextField(labelWithString: "PaneruBar Settings")
    title.font = .systemFont(ofSize: 13, weight: .semibold)
    header.addArrangedSubview(title)

    errorLabel.font = .systemFont(ofSize: 11)
    errorLabel.textColor = .systemRed
    errorLabel.isHidden = true
    errorLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
    header.addArrangedSubview(errorLabel)

    let restoreDefaults = NSButton(
      title: "Restore Defaults",
      target: self,
      action: #selector(restoreDefaultsClicked)
    )
    restoreDefaults.bezelStyle = .inline
    restoreDefaults.controlSize = .small
    restoreDefaults.toolTip = "Restore all settings to their defaults"
    header.addArrangedSubview(restoreDefaults)

    let close = iconButton(symbol: "xmark", description: "Close")
    close.target = self
    close.action = #selector(closeClicked)
    header.addArrangedSubview(close)

    configureTargets()
    configureFloatingStylePopup()
    for colorWell in [
      backgroundColorWell,
      borderColorWell,
      focusColorWell,
      activeWorkspaceColorWell,
    ] {
      if #available(macOS 14, *) {
        colorWell.supportsAlpha = true
      } else {
        NSColorPanel.shared.showsAlpha = true
      }
      colorWell.translatesAutoresizingMaskIntoConstraints = false
      colorWell.widthAnchor.constraint(equalToConstant: 44).isActive = true
      colorWell.heightAnchor.constraint(equalToConstant: 24).isActive = true
    }

    workspaceLabelsStack.orientation = .vertical
    workspaceLabelsStack.alignment = .leading
    workspaceLabelsStack.spacing = 7

    let content = NSStackView(views: [
      section(
        "Bar",
        rows: [
          sliderRow("Height", slider: heightSlider, value: heightValue),
          sliderRow("Vertical padding", slider: verticalPaddingSlider, value: verticalPaddingValue),
          sliderRow("Workspace gap", slider: workspaceSpacingSlider, value: workspaceSpacingValue),
          sliderRow("Window gap", slider: windowSpacingSlider, value: windowSpacingValue),
          sliderRow(
            "Horizontal padding", slider: horizontalPaddingSlider, value: horizontalPaddingValue),
        ]),
      separator(),
      section(
        "Appearance",
        rows: [
          labeledRow("Background", control: backgroundColorWell),
          labeledRow("Border", control: borderColorWell),
          sliderRow("Border width", slider: borderWidthSlider, value: borderWidthValue),
          sliderRow("Corner radius", slider: cornerRadiusSlider, value: cornerRadiusValue),
          checkboxRow(shadowButton),
          labeledRow("Focus indicator color", control: focusColorWell),
          sliderRow("Indicator thickness", slider: focusRingWidthSlider, value: focusRingWidthValue),
          labeledRow("Selected background", control: activeWorkspaceColorWell),
          labeledRow("Floating icons", control: floatingStylePopup),
          checkboxRow(focusRingButton),
        ]),
      separator(),
      section("Workspace labels", rows: [workspaceLabelsStack]),
      separator(),
      section(
        "Animation",
        rows: [
          labeledRow("Style", control: animationControl),
          sliderRow("Duration", slider: durationSlider, value: durationValue),
        ]),
    ])
    content.orientation = .vertical
    content.alignment = .leading
    content.spacing = 12
    content.translatesAutoresizingMaskIntoConstraints = false

    let document = NSView()
    document.translatesAutoresizingMaskIntoConstraints = false
    document.addSubview(content)
    let scroll = NSScrollView()
    scroll.drawsBackground = false
    scroll.borderType = .noBorder
    scroll.hasVerticalScroller = true
    scroll.autohidesScrollers = true
    scroll.documentView = document
    scroll.translatesAutoresizingMaskIntoConstraints = false

    header.translatesAutoresizingMaskIntoConstraints = false
    addSubview(header)
    addSubview(scroll)
    NSLayoutConstraint.activate([
      header.leadingAnchor.constraint(equalTo: leadingAnchor),
      header.trailingAnchor.constraint(equalTo: trailingAnchor),
      header.topAnchor.constraint(equalTo: topAnchor),
      header.heightAnchor.constraint(equalToConstant: 26),
      scroll.leadingAnchor.constraint(equalTo: leadingAnchor),
      scroll.trailingAnchor.constraint(equalTo: trailingAnchor),
      scroll.topAnchor.constraint(equalTo: header.bottomAnchor, constant: 8),
      scroll.bottomAnchor.constraint(equalTo: bottomAnchor),
      document.widthAnchor.constraint(equalTo: scroll.contentView.widthAnchor),
      content.leadingAnchor.constraint(equalTo: document.leadingAnchor),
      content.trailingAnchor.constraint(equalTo: document.trailingAnchor, constant: -8),
      content.topAnchor.constraint(equalTo: document.topAnchor),
      content.bottomAnchor.constraint(equalTo: document.bottomAnchor),
    ])
  }

  private func configureTargets() {
    let controls: [NSControl] = [
      heightSlider,
      verticalPaddingSlider,
      workspaceSpacingSlider,
      windowSpacingSlider,
      horizontalPaddingSlider,
      backgroundColorWell,
      borderColorWell,
      borderWidthSlider,
      cornerRadiusSlider,
      shadowButton,
      focusColorWell,
      activeWorkspaceColorWell,
      floatingStylePopup,
      focusRingButton,
      focusRingWidthSlider,
      animationControl,
      durationSlider,
    ]
    for control in controls {
      control.target = self
      control.action = #selector(controlChanged(_:))
      control.controlSize = .small
    }
    for slider in [
      heightSlider,
      verticalPaddingSlider,
      workspaceSpacingSlider,
      windowSpacingSlider,
      horizontalPaddingSlider,
    ] {
      slider.allowsTickMarkValuesOnly = true
      slider.numberOfTickMarks = Int(slider.maxValue - slider.minValue) + 1
      slider.isContinuous = false
    }
    focusRingWidthSlider.numberOfTickMarks = 15
    focusRingWidthSlider.allowsTickMarkValuesOnly = true
    focusRingWidthSlider.isContinuous = false
    borderWidthSlider.numberOfTickMarks = 17
    borderWidthSlider.allowsTickMarkValuesOnly = true
    borderWidthSlider.isContinuous = false
    cornerRadiusSlider.numberOfTickMarks = 17
    cornerRadiusSlider.allowsTickMarkValuesOnly = true
    cornerRadiusSlider.isContinuous = false
    durationSlider.isContinuous = false
  }

  private func configureFloatingStylePopup() {
    for style in FloatingIconStyle.allCases {
      floatingStylePopup.addItem(withTitle: style.rawValue.capitalized)
      floatingStylePopup.lastItem?.representedObject = style.rawValue
    }
  }

  @objc private func closeClicked() { onClose() }

  @objc private func restoreDefaultsClicked() {
    saveAndApply(BarPreferences())
  }

  @objc private func controlChanged(_ sender: NSControl) {
    guard !applying else { return }
    var next = preferences
    if sender === heightSlider {
      next.barHeight = CGFloat(heightSlider.doubleValue.rounded())
      next.verticalPadding = min(
        next.verticalPadding,
        next.maximumVerticalPadding
      )
    } else if sender === verticalPaddingSlider {
      next.verticalPadding = CGFloat(verticalPaddingSlider.doubleValue.rounded())
    } else if sender === workspaceSpacingSlider {
      next.itemSpacing = CGFloat(workspaceSpacingSlider.doubleValue.rounded())
    } else if sender === windowSpacingSlider {
      next.windowSpacing = CGFloat(windowSpacingSlider.doubleValue.rounded())
    } else if sender === horizontalPaddingSlider {
      next.horizontalPadding = CGFloat(horizontalPaddingSlider.doubleValue.rounded())
    } else if sender === backgroundColorWell {
      next.backgroundColorHex = backgroundColorWell.color.paneruHexString ?? next.backgroundColorHex
    } else if sender === borderColorWell {
      next.borderColorHex = borderColorWell.color.paneruHexString ?? next.borderColorHex
    } else if sender === borderWidthSlider {
      next.borderWidth = CGFloat((borderWidthSlider.doubleValue * 4).rounded() / 4)
    } else if sender === cornerRadiusSlider {
      next.cornerRadius = CGFloat(cornerRadiusSlider.doubleValue.rounded())
    } else if sender === shadowButton {
      next.showsShadow = shadowButton.state == .on
    } else if sender === focusColorWell {
      next.selectionColorHex = focusColorWell.color.paneruHexString ?? next.selectionColorHex
    } else if sender === activeWorkspaceColorWell {
      next.activeWorkspaceColorHex =
        activeWorkspaceColorWell.color.paneruHexString ?? next.activeWorkspaceColorHex
    } else if sender === floatingStylePopup {
      let raw = floatingStylePopup.selectedItem?.representedObject as? String
      next.floatingIconStyle =
        raw.flatMap(FloatingIconStyle.init(rawValue:)) ?? next.floatingIconStyle
    } else if sender === focusRingButton {
      next.showsFocusRing = focusRingButton.state == .on
    } else if sender === focusRingWidthSlider {
      next.focusRingWidth = CGFloat(
        (focusRingWidthSlider.doubleValue * 4).rounded() / 4
      )
    } else if sender === animationControl {
      let styles: [BarAnimationStyle] = [.none, .smooth, .spring]
      guard styles.indices.contains(animationControl.selectedSegment) else { return }
      next.animationStyle = styles[animationControl.selectedSegment]
    } else if sender === durationSlider {
      next.animationDuration = (durationSlider.doubleValue * 100).rounded() / 100
    } else if let entry = workspaceLabelFields.first(where: { $0.value === sender }) {
      let label = entry.value.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
      if label.isEmpty {
        next.workspaceLabels.removeValue(forKey: entry.key)
      } else {
        next.workspaceLabels[entry.key] = label
      }
    } else {
      return
    }

    saveAndApply(next)
  }

  private func saveAndApply(_ next: BarPreferences) {
    do {
      try onChange(next)
      apply(next, workspaceNumbers: workspaceNumbers)
      errorLabel.isHidden = true
      errorLabel.toolTip = nil
    } catch {
      apply(preferences, workspaceNumbers: workspaceNumbers)
      errorLabel.stringValue = "Save failed"
      errorLabel.toolTip = error.localizedDescription
      errorLabel.isHidden = false
    }
  }

  private func updateValueLabels() {
    heightValue.stringValue = "\(Int(heightSlider.doubleValue.rounded()))"
    verticalPaddingValue.stringValue = "\(Int(verticalPaddingSlider.doubleValue.rounded()))"
    workspaceSpacingValue.stringValue = "\(Int(workspaceSpacingSlider.doubleValue.rounded()))"
    windowSpacingValue.stringValue = "\(Int(windowSpacingSlider.doubleValue.rounded()))"
    horizontalPaddingValue.stringValue = "\(Int(horizontalPaddingSlider.doubleValue.rounded()))"
    borderWidthValue.stringValue = String(format: "%.2g", borderWidthSlider.doubleValue)
    cornerRadiusValue.stringValue = "\(Int(cornerRadiusSlider.doubleValue.rounded()))"
    focusRingWidthValue.stringValue = String(format: "%.2g", focusRingWidthSlider.doubleValue)
    durationValue.stringValue = String(format: "%.2fs", durationSlider.doubleValue)
  }

  private func updateEnabledStates() {
    focusRingWidthSlider.isEnabled = focusRingButton.state == .on
    borderColorWell.isEnabled = borderWidthSlider.doubleValue > 0
    durationSlider.isEnabled = animationControl.selectedSegment != 0
  }

  private func rebuildWorkspaceLabelFields() {
    let numbers = Set(workspaceNumbers).union(preferences.workspaceLabels.keys).sorted()
    guard numbers != workspaceLabelFields.keys.sorted() else {
      for number in numbers {
        workspaceLabelFields[number]?.stringValue = preferences.workspaceLabels[number] ?? ""
      }
      return
    }

    for view in workspaceLabelsStack.arrangedSubviews {
      workspaceLabelsStack.removeArrangedSubview(view)
      view.removeFromSuperview()
    }
    workspaceLabelFields.removeAll()
    for number in numbers {
      let field = NSTextField(string: preferences.workspaceLabels[number] ?? "")
      field.placeholderString = String(number)
      field.controlSize = .small
      field.target = self
      field.action = #selector(controlChanged(_:))
      field.translatesAutoresizingMaskIntoConstraints = false
      field.widthAnchor.constraint(equalToConstant: 190).isActive = true
      workspaceLabelFields[number] = field
      workspaceLabelsStack.addArrangedSubview(
        labeledRow("Workspace \(number)", control: field)
      )
    }
  }

  private func section(_ title: String, rows: [NSView]) -> NSView {
    let heading = NSTextField(labelWithString: title)
    heading.font = .systemFont(ofSize: 11, weight: .semibold)
    heading.textColor = .secondaryLabelColor
    let stack = NSStackView(views: [heading] + rows)
    stack.orientation = .vertical
    stack.alignment = .leading
    stack.spacing = 7
    return stack
  }

  private func labeledRow(_ title: String, control: NSView) -> NSView {
    let label = NSTextField(labelWithString: title)
    label.font = .systemFont(ofSize: 12)
    label.translatesAutoresizingMaskIntoConstraints = false
    label.widthAnchor.constraint(equalToConstant: 118).isActive = true
    let row = NSStackView(views: [label, control])
    row.orientation = .horizontal
    row.alignment = .centerY
    row.spacing = 8
    row.translatesAutoresizingMaskIntoConstraints = false
    row.widthAnchor.constraint(equalToConstant: 330).isActive = true
    return row
  }

  private func sliderRow(_ title: String, slider: NSSlider, value: NSTextField) -> NSView {
    slider.translatesAutoresizingMaskIntoConstraints = false
    slider.widthAnchor.constraint(equalToConstant: 145).isActive = true
    return labeledRow(title, control: NSStackView(views: [slider, value]))
  }

  private func checkboxRow(_ checkbox: NSButton) -> NSView {
    let spacer = NSView()
    spacer.translatesAutoresizingMaskIntoConstraints = false
    spacer.widthAnchor.constraint(equalToConstant: 118).isActive = true
    let row = NSStackView(views: [spacer, checkbox])
    row.orientation = .horizontal
    row.alignment = .centerY
    row.spacing = 8
    row.translatesAutoresizingMaskIntoConstraints = false
    row.widthAnchor.constraint(equalToConstant: 330).isActive = true
    return row
  }

  private func separator() -> NSView {
    let box = NSBox()
    box.boxType = .separator
    box.translatesAutoresizingMaskIntoConstraints = false
    box.widthAnchor.constraint(equalToConstant: 330).isActive = true
    return box
  }

  private static func slider(_ range: ClosedRange<Double>) -> NSSlider {
    NSSlider(
      value: range.lowerBound, minValue: range.lowerBound, maxValue: range.upperBound, target: nil,
      action: nil)
  }

  private static func valueLabel() -> NSTextField {
    let label = NSTextField(labelWithString: "")
    label.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)
    label.alignment = .right
    label.textColor = .secondaryLabelColor
    label.translatesAutoresizingMaskIntoConstraints = false
    label.widthAnchor.constraint(equalToConstant: 38).isActive = true
    return label
  }

  private func iconButton(symbol: String, description: String) -> NSButton {
    let image =
      NSImage(systemSymbolName: symbol, accessibilityDescription: description) ?? NSImage()
    let button = NSButton(image: image, target: nil, action: nil)
    button.isBordered = false
    button.bezelStyle = .inline
    button.toolTip = description
    button.translatesAutoresizingMaskIntoConstraints = false
    button.widthAnchor.constraint(equalToConstant: 24).isActive = true
    button.heightAnchor.constraint(equalToConstant: 24).isActive = true
    return button
  }
}

extension NSColor {
  var paneruHexString: String? {
    guard let color = usingColorSpace(.sRGB) else { return nil }
    let red = Int((color.redComponent * 255).rounded())
    let green = Int((color.greenComponent * 255).rounded())
    let blue = Int((color.blueComponent * 255).rounded())
    let alpha = Int((color.alphaComponent * 255).rounded())
    return String(format: "#%02X%02X%02X%02X", red, green, blue, alpha)
  }
}
