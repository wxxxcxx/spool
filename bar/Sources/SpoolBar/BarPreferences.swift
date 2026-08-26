import AppKit

enum FloatingIconStyle: String, CaseIterable {
  case badge
  case raised
  case dimmed
}

enum BarAnimationStyle: String, CaseIterable {
  case none
  case smooth
  case spring
}

struct BarPreferences: Equatable {
  var barHeight: CGFloat
  var verticalPadding: CGFloat
  var itemSpacing: CGFloat
  var windowSpacing: CGFloat
  var horizontalPadding: CGFloat
  var backgroundColorHex: String
  var borderColorHex: String
  var borderWidth: CGFloat
  var cornerRadius: CGFloat
  var showsShadow: Bool
  var selectionColorHex: String
  var activeWorkspaceColorHex: String
  var floatingIconStyle: FloatingIconStyle
  var workspaceLabels: [UInt32: String]
  var showsFocusRing: Bool
  var focusRingWidth: CGFloat
  var animationStyle: BarAnimationStyle
  var animationDuration: TimeInterval

  init(
    barHeight: CGFloat = 28,
    verticalPadding: CGFloat = 3,
    itemSpacing: CGFloat = 5,
    windowSpacing: CGFloat = 3,
    horizontalPadding: CGFloat = 7,
    backgroundColorHex: String = "#1C1C1ED9",
    borderColorHex: String = "#FFFFFF33",
    borderWidth: CGFloat = 0.5,
    cornerRadius: CGFloat = 8,
    showsShadow: Bool = true,
    selectionColorHex: String = "#0A84FFFF",
    activeWorkspaceColorHex: String = "#0A84FF47",
    floatingIconStyle: FloatingIconStyle = .badge,
    workspaceLabels: [UInt32: String] = [:],
    showsFocusRing: Bool = true,
    focusRingWidth: CGFloat = 1.5,
    animationStyle: BarAnimationStyle = .spring,
    animationDuration: TimeInterval = 0.24
  ) {
    self.barHeight = barHeight
    self.verticalPadding = verticalPadding
    self.itemSpacing = itemSpacing
    self.windowSpacing = windowSpacing
    self.horizontalPadding = horizontalPadding
    self.backgroundColorHex = backgroundColorHex
    self.borderColorHex = borderColorHex
    self.borderWidth = borderWidth
    self.cornerRadius = cornerRadius
    self.showsShadow = showsShadow
    self.selectionColorHex = selectionColorHex
    self.activeWorkspaceColorHex = activeWorkspaceColorHex
    self.floatingIconStyle = floatingIconStyle
    self.workspaceLabels = workspaceLabels
    self.showsFocusRing = showsFocusRing
    self.focusRingWidth = focusRingWidth
    self.animationStyle = animationStyle
    self.animationDuration = animationDuration
  }

  func workspaceLabel(for number: UInt32) -> String {
    if let replacement = workspaceLabels[number]?.trimmingCharacters(in: .whitespacesAndNewlines),
      !replacement.isEmpty
    {
      return replacement
    }
    return String(number)
  }

  var effectiveVerticalPadding: CGFloat {
    min(max(0, verticalPadding), maximumVerticalPadding)
  }

  var maximumVerticalPadding: CGFloat {
    floor(max(0, (barHeight - 14) / 2))
  }

  var iconButtonSize: CGFloat {
    barHeight - 2 * effectiveVerticalPadding
  }

  var iconSize: CGFloat {
    max(10, iconButtonSize - 4)
  }
}

enum BarTOML {
  private static let topLevelKeyOrder = [
    "height",
    "vertical_padding",
    "workspace_spacing",
    "window_spacing",
    "horizontal_padding",
    "background_color",
    "border_color",
    "border_width",
    "corner_radius",
    "show_shadow",
    "selection_color",
    "active_workspace_color",
    "floating_icon_style",
    "show_focus_ring",
    "focus_ring_width",
    "animation_style",
    "animation_duration",
  ]

  static let template = """
    # SpoolBar reloads this file automatically.
    height = 28
    vertical_padding = 3
    workspace_spacing = 5
    window_spacing = 3
    horizontal_padding = 7
    background_color = "#1C1C1ED9"
    border_color = "#FFFFFF33"
    border_width = 0.5
    corner_radius = 8
    show_shadow = true
    selection_color = "#0A84FFFF"
    active_workspace_color = "#0A84FF47"
    floating_icon_style = "badge" # badge, raised, dimmed
    show_focus_ring = true
    focus_ring_width = 1.5
    animation_style = "spring" # none, smooth, spring
    animation_duration = 0.24

    [workspace_labels]
    # 1 = "work"
    # 2 = "chat"
    """

  static func parse(_ source: String) throws -> BarPreferences {
    var values: [String: String] = [:]
    var labels: [UInt32: String] = [:]
    var section = ""

    for (offset, original) in source.split(whereSeparator: \.isNewline).enumerated() {
      let lineNumber = offset + 1
      let line = stripComment(String(original)).trimmingCharacters(in: .whitespaces)
      guard !line.isEmpty else { continue }
      if line.hasPrefix("[") && line.hasSuffix("]") {
        section = String(line.dropFirst().dropLast()).trimmingCharacters(in: .whitespaces)
        continue
      }
      guard let assignment = splitAssignment(line) else {
        throw ParseError(line: lineNumber, message: "expected key = value")
      }
      let key = assignment.0.trimmingCharacters(in: .whitespaces)
      let value = assignment.1.trimmingCharacters(in: .whitespaces)
      if section == "workspace_labels" {
        guard let number = UInt32(key) else {
          throw ParseError(line: lineNumber, message: "workspace label key must be a number")
        }
        labels[number] = try parseString(value, line: lineNumber)
      } else if section.isEmpty {
        values[key] = value
      }
    }

    let barHeight = CGFloat(try number("height", in: values, default: 28))
      .clamped(to: 22...44)
    let maximumPadding = floor(max(0, (barHeight - 14) / 2))
    let verticalPadding = CGFloat(
      try number("vertical_padding", in: values, default: 3)
    ).clamped(to: 0...maximumPadding)
    let selectionColor = try color(
      "selection_color",
      in: values,
      default: "#0A84FFFF"
    )
    let activeColor = try color(
      "active_workspace_color",
      in: values,
      default: "#0A84FF47"
    )
    let backgroundColor = try color(
      "background_color",
      in: values,
      default: "#1C1C1ED9"
    )
    let borderColor = try color(
      "border_color",
      in: values,
      default: "#FFFFFF33"
    )
    return BarPreferences(
      barHeight: barHeight,
      verticalPadding: verticalPadding,
      itemSpacing: CGFloat(try number("workspace_spacing", in: values, default: 5)).clamped(
        to: 2...14),
      windowSpacing: CGFloat(try number("window_spacing", in: values, default: 3))
        .clamped(to: 0...14),
      horizontalPadding: CGFloat(try number("horizontal_padding", in: values, default: 7)).clamped(
        to: 4...20),
      backgroundColorHex: backgroundColor,
      borderColorHex: borderColor,
      borderWidth: CGFloat(try number("border_width", in: values, default: 0.5))
        .clamped(to: 0...4),
      cornerRadius: CGFloat(try number("corner_radius", in: values, default: 8))
        .clamped(to: 0...16),
      showsShadow: try bool("show_shadow", in: values, default: true),
      selectionColorHex: selectionColor,
      activeWorkspaceColorHex: activeColor,
      floatingIconStyle: try enumValue("floating_icon_style", in: values, default: .badge),
      workspaceLabels: labels,
      showsFocusRing: try bool("show_focus_ring", in: values, default: true),
      focusRingWidth: CGFloat(
        try number("focus_ring_width", in: values, default: 1.5)
      ).clamped(to: 0.5...4),
      animationStyle: try enumValue("animation_style", in: values, default: .spring),
      animationDuration: try number("animation_duration", in: values, default: 0.24)
        .clamped(to: 0.08...0.6)
    )
  }

  static func updating(_ source: String, with preferences: BarPreferences) -> String {
    let rendered = renderedTopLevelValues(preferences)
    let hadTrailingNewline = source.hasSuffix("\n")
    var lines = source.components(separatedBy: .newlines)
    if hadTrailingNewline, lines.last == "" { lines.removeLast() }

    var section = ""
    var firstSectionIndex: Int?
    var seen = Set<String>()

    for index in lines.indices {
      let code = stripComment(lines[index]).trimmingCharacters(in: .whitespaces)
      if code.hasPrefix("[") && code.hasSuffix("]") {
        if firstSectionIndex == nil { firstSectionIndex = index }
        section = String(code.dropFirst().dropLast())
          .trimmingCharacters(in: .whitespaces)
        continue
      }
      guard section.isEmpty, let assignment = splitAssignment(code) else { continue }
      let key = assignment.0.trimmingCharacters(in: .whitespaces)
      let obsolete = [
        "position",
        "split_at_notch",
        "notch_workspace_layout",
        "inactive_workspace_color",
        "show_workspace_label",
        "collapse_inactive_workspaces",
        "stack_rotation_step",
        "stack_rotation_limit",
      ]
      if obsolete.contains(key) {
        lines[index] = ""
        continue
      }
      guard let value = rendered[key] else { continue }
      let indentation = lines[index].prefix { $0 == " " || $0 == "\t" }
      let comment = commentSuffix(in: lines[index]).map { " \($0)" } ?? ""
      lines[index] = "\(indentation)\(key) = \(value)\(comment)"
      seen.insert(key)
    }

    let missing =
      topLevelKeyOrder
      .filter { !seen.contains($0) }
      .compactMap { key in rendered[key].map { "\(key) = \($0)" } }
    if !missing.isEmpty {
      let insertionIndex = firstSectionIndex ?? lines.count
      var inserted = missing
      if insertionIndex > 0, !lines[insertionIndex - 1].isEmpty {
        inserted.insert("", at: 0)
      }
      if insertionIndex < lines.count, !lines[insertionIndex].isEmpty {
        inserted.append("")
      }
      lines.insert(contentsOf: inserted, at: insertionIndex)
    }

    lines = updatingWorkspaceLabels(lines, with: preferences.workspaceLabels)

    let result = lines.joined(separator: "\n")
    return hadTrailingNewline || !result.isEmpty ? result + "\n" : result
  }

  private static func updatingWorkspaceLabels(
    _ lines: [String],
    with labels: [UInt32: String]
  ) -> [String] {
    guard
      let sectionStart = lines.firstIndex(where: {
        stripComment($0).trimmingCharacters(in: .whitespaces) == "[workspace_labels]"
      })
    else {
      var result = lines
      if !result.isEmpty, result.last != "" { result.append("") }
      result.append("[workspace_labels]")
      result.append(
        contentsOf: labels.keys.sorted().compactMap { number in
          labels[number].map { "\(number) = \(quoted($0))" }
        })
      return result
    }

    let sectionEnd =
      lines[(sectionStart + 1)...].firstIndex(where: {
        let code = stripComment($0).trimmingCharacters(in: .whitespaces)
        return code.hasPrefix("[") && code.hasSuffix("]")
      }) ?? lines.endIndex
    var seen = Set<UInt32>()
    var body: [String] = []
    for line in lines[(sectionStart + 1)..<sectionEnd] {
      let code = stripComment(line).trimmingCharacters(in: .whitespaces)
      guard let assignment = splitAssignment(code),
        let number = UInt32(assignment.0.trimmingCharacters(in: .whitespaces))
      else {
        body.append(line)
        continue
      }
      guard let label = labels[number] else { continue }
      let indentation = line.prefix { $0 == " " || $0 == "\t" }
      let comment = commentSuffix(in: line).map { " \($0)" } ?? ""
      body.append("\(indentation)\(number) = \(quoted(label))\(comment)")
      seen.insert(number)
    }

    let missing = labels.keys.sorted().filter { !seen.contains($0) }.compactMap { number in
      labels[number].map { "\(number) = \(quoted($0))" }
    }
    let insertionIndex =
      body.lastIndex(where: {
        !$0.trimmingCharacters(in: .whitespaces).isEmpty
      }).map { $0 + 1 } ?? 0
    body.insert(contentsOf: missing, at: insertionIndex)

    return Array(lines[..<sectionStart]) + [lines[sectionStart]] + body
      + Array(lines[sectionEnd...])
  }

  private static func renderedTopLevelValues(
    _ preferences: BarPreferences
  ) -> [String: String] {
    [
      "height": numberString(preferences.barHeight),
      "vertical_padding": numberString(preferences.verticalPadding),
      "workspace_spacing": numberString(preferences.itemSpacing),
      "window_spacing": numberString(preferences.windowSpacing),
      "horizontal_padding": numberString(preferences.horizontalPadding),
      "background_color": quoted(preferences.backgroundColorHex),
      "border_color": quoted(preferences.borderColorHex),
      "border_width": numberString(preferences.borderWidth),
      "corner_radius": numberString(preferences.cornerRadius),
      "show_shadow": String(preferences.showsShadow),
      "selection_color": quoted(preferences.selectionColorHex),
      "active_workspace_color": quoted(preferences.activeWorkspaceColorHex),
      "floating_icon_style": quoted(preferences.floatingIconStyle.rawValue),
      "show_focus_ring": String(preferences.showsFocusRing),
      "focus_ring_width": numberString(preferences.focusRingWidth),
      "animation_style": quoted(preferences.animationStyle.rawValue),
      "animation_duration": numberString(preferences.animationDuration),
    ]
  }

  private static func numberString<T: BinaryFloatingPoint>(_ value: T) -> String {
    let double = Double(value)
    if double.rounded() == double { return String(Int(double)) }
    return String(format: "%.2f", locale: Locale(identifier: "en_US_POSIX"), double)
      .replacingOccurrences(of: "0+$", with: "", options: .regularExpression)
      .replacingOccurrences(of: "\\.$", with: "", options: .regularExpression)
  }

  private static func quoted(_ value: String) -> String {
    guard let data = try? JSONEncoder().encode(value),
      let encoded = String(data: data, encoding: .utf8)
    else { return "\"\"" }
    return encoded
  }

  private static func number(
    _ key: String,
    in values: [String: String],
    default fallback: Double
  ) throws -> Double {
    guard let raw = values[key] else { return fallback }
    guard let value = Double(raw) else {
      throw ParseError(line: 0, message: "\(key) must be a number")
    }
    return value
  }

  private static func bool(
    _ key: String,
    in values: [String: String],
    default fallback: Bool
  ) throws -> Bool {
    guard let raw = values[key] else { return fallback }
    switch raw {
    case "true": return true
    case "false": return false
    default: throw ParseError(line: 0, message: "\(key) must be true or false")
    }
  }

  private static func string(
    _ key: String,
    in values: [String: String],
    default fallback: String
  ) throws -> String {
    guard let raw = values[key] else { return fallback }
    return try parseString(raw, line: 0)
  }

  private static func color(
    _ key: String,
    in values: [String: String],
    default fallback: String
  ) throws -> String {
    let value = try string(key, in: values, default: fallback)
    guard NSColor(spoolHex: value) != nil else {
      throw ParseError(line: 0, message: "\(key) must be #RRGGBB or #RRGGBBAA")
    }
    return value
  }

  private static func enumValue<T: RawRepresentable>(
    _ key: String,
    in values: [String: String],
    default fallback: T
  ) throws -> T where T.RawValue == String {
    let raw = try string(key, in: values, default: fallback.rawValue)
    guard let value = T(rawValue: raw) else {
      throw ParseError(line: 0, message: "unsupported value for \(key): \(raw)")
    }
    return value
  }

  private static func parseString(_ raw: String, line: Int) throws -> String {
    guard raw.first == "\"", raw.last == "\"",
      let data = "[\(raw)]".data(using: .utf8),
      let decoded = try? JSONSerialization.jsonObject(with: data) as? [String],
      let value = decoded.first
    else {
      throw ParseError(line: line, message: "string values must use double quotes")
    }
    return value
  }

  private static func stripComment(_ line: String) -> String {
    guard let index = commentIndex(in: line) else { return line }
    return String(line[..<index])
  }

  private static func commentSuffix(in line: String) -> String? {
    commentIndex(in: line).map { String(line[$0...]) }
  }

  private static func commentIndex(in line: String) -> String.Index? {
    var quoted = false
    var escaped = false
    for index in line.indices {
      let character = line[index]
      if character == "\"", !escaped { quoted.toggle() }
      if character == "#", !quoted { return index }
      escaped = character == "\\" && !escaped
      if character != "\\" { escaped = false }
    }
    return nil
  }

  private static func splitAssignment(_ line: String) -> (String, String)? {
    var quoted = false
    var escaped = false
    for index in line.indices {
      let character = line[index]
      if character == "\"", !escaped { quoted.toggle() }
      if character == "=", !quoted {
        return (String(line[..<index]), String(line[line.index(after: index)...]))
      }
      escaped = character == "\\" && !escaped
      if character != "\\" { escaped = false }
    }
    return nil
  }

  struct ParseError: LocalizedError {
    let line: Int
    let message: String

    var errorDescription: String? {
      line > 0 ? "line \(line): \(message)" : message
    }
  }
}

final class BarConfigurationStore {
  let url: URL
  private let legacyURL: URL?
  private(set) var preferences = BarPreferences()
  var onChange: ((BarPreferences) -> Void)?
  private var timer: Timer?
  private var signature = ""

  convenience init() {
    self.init(
      url: BarConfigurationStore.defaultURL(),
      legacyURL: BarConfigurationStore.defaultLegacyURL()
    )
  }

  init(url: URL, legacyURL: URL? = nil) {
    self.url = url
    self.legacyURL = legacyURL
  }

  func start() {
    ensureConfigurationExists()
    reload(force: true)
    timer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
      self?.reload(force: false)
    }
  }

  func stop() {
    timer?.invalidate()
    timer = nil
  }

  func save(_ next: BarPreferences) throws {
    let source = (try? String(contentsOf: url, encoding: .utf8)) ?? BarTOML.template
    let updated = BarTOML.updating(source, with: next)
    try updated.write(to: url, atomically: true, encoding: .utf8)
    preferences = next
    signature = fileSignature()
    onChange?(next)
  }

  private func ensureConfigurationExists() {
    guard !FileManager.default.fileExists(atPath: url.path) else { return }
    do {
      try FileManager.default.createDirectory(
        at: url.deletingLastPathComponent(),
        withIntermediateDirectories: true
      )
      if let legacyURL,
        FileManager.default.fileExists(atPath: legacyURL.path)
      {
        try FileManager.default.moveItem(at: legacyURL, to: url)
        NSLog("SpoolBar: moved configuration from %@ to %@", legacyURL.path, url.path)
        return
      }
      try BarTOML.template.write(to: url, atomically: true, encoding: .utf8)
    } catch {
      NSLog("SpoolBar: unable to create %@: %@", url.path, error.localizedDescription)
    }
  }

  private func reload(force: Bool) {
    let nextSignature = fileSignature()
    guard force || nextSignature != signature else { return }
    signature = nextSignature
    do {
      let source = try String(contentsOf: url, encoding: .utf8)
      let next = try BarTOML.parse(source)
      guard force || next != preferences else { return }
      preferences = next
      onChange?(next)
    } catch {
      NSLog("SpoolBar: unable to load %@: %@", url.path, error.localizedDescription)
    }
  }

  private func fileSignature() -> String {
    let attributes = try? FileManager.default.attributesOfItem(atPath: url.path)
    let modified = (attributes?[.modificationDate] as? Date)?.timeIntervalSince1970 ?? 0
    let size = attributes?[.size] as? NSNumber ?? 0
    return "\(modified):\(size)"
  }

  private static func defaultURL() -> URL {
    if let configured = ProcessInfo.processInfo.environment["SPOOL_BAR_CONFIG"],
      !configured.isEmpty
    {
      return URL(fileURLWithPath: (configured as NSString).expandingTildeInPath)
    }
    return spoolConfigurationDirectory().appendingPathComponent("bar.toml")
  }

  private static func defaultLegacyURL() -> URL? {
    guard ProcessInfo.processInfo.environment["SPOOL_BAR_CONFIG"]?.isEmpty != false else {
      return nil
    }
    return FileManager.default.homeDirectoryForCurrentUser
      .appendingPathComponent(".config/spool-bar/config.toml")
  }

  private static func spoolConfigurationDirectory() -> URL {
    if let configured = ProcessInfo.processInfo.environment["XDG_CONFIG_HOME"],
      !configured.isEmpty
    {
      return URL(fileURLWithPath: (configured as NSString).expandingTildeInPath)
        .appendingPathComponent("spool", isDirectory: true)
    }
    return FileManager.default.homeDirectoryForCurrentUser
      .appendingPathComponent(".config/spool", isDirectory: true)
  }
}

extension NSColor {
  convenience init?(spoolHex value: String) {
    let hex = value.trimmingCharacters(in: CharacterSet(charactersIn: "#"))
    guard hex.count == 6 || hex.count == 8, let raw = UInt64(hex, radix: 16) else { return nil }
    let hasAlpha = hex.count == 8
    let red = CGFloat((raw >> (hasAlpha ? 24 : 16)) & 0xff) / 255
    let green = CGFloat((raw >> (hasAlpha ? 16 : 8)) & 0xff) / 255
    let blue = CGFloat((raw >> (hasAlpha ? 8 : 0)) & 0xff) / 255
    let alpha = hasAlpha ? CGFloat(raw & 0xff) / 255 : 1
    self.init(srgbRed: red, green: green, blue: blue, alpha: alpha)
  }

}

extension Comparable {
  fileprivate func clamped(to range: ClosedRange<Self>) -> Self {
    min(max(self, range.lowerBound), range.upperBound)
  }
}
