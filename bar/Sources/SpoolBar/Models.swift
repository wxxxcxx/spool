import Foundation

struct SpoolStateDocument: Decodable, Equatable {
    let version: UInt32
    let timestamp: UInt64
    let active: ActiveState
    let capabilities: SpaceCapabilities
    let displays: [DisplayState]
    let spaces: [SpaceState]

    private enum CodingKeys: String, CodingKey {
        case version, timestamp, active, capabilities, displays
        case spaces
        case virtualWorkspaces = "virtual_workspaces"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        version = try container.decode(UInt32.self, forKey: .version)
        timestamp = try container.decode(UInt64.self, forKey: .timestamp)
        active = try container.decode(ActiveState.self, forKey: .active)
        capabilities = try container.decodeIfPresent(SpaceCapabilities.self, forKey: .capabilities)
            ?? SpaceCapabilities()
        displays = try container.decodeIfPresent([DisplayState].self, forKey: .displays) ?? []
        if let nativeSpaces = try container.decodeIfPresent([SpaceState].self, forKey: .spaces) {
            spaces = nativeSpaces
        } else {
            spaces = try container.decode([SpaceState].self, forKey: .virtualWorkspaces)
        }
    }

    func display(_ displayID: UInt32) -> DisplayState? {
        displays.first { $0.displayID == displayID }
    }

    func spaces(on display: DisplayState) -> [SpaceState] {
        spaces
            .filter {
                guard $0.displayID == display.displayID else { return false }
                guard version < 3, let visibleID = display.visibleSpaceID else { return true }
                return $0.spaceID == visibleID
            }
            .sorted { $0.number < $1.number }
    }

}

struct SpaceCapabilities: Decodable, Equatable {
    let moveWindows: Bool
    let focus: Bool
    let create: Bool
    let delete: Bool

    init(moveWindows: Bool = false, focus: Bool = false, create: Bool = false, delete: Bool = false) {
        self.moveWindows = moveWindows
        self.focus = focus
        self.create = create
        self.delete = delete
    }

    private enum CodingKeys: String, CodingKey {
        case moveWindows = "move_windows"
        case focus, create, delete
    }
}

struct ActiveState: Decodable, Equatable {
    let displayID: UInt32?
    let spaceID: UInt64?
    let virtualWorkspaceNumber: UInt32?
    let focusedWindowID: Int32?

    private enum CodingKeys: String, CodingKey {
        case displayID = "display_id"
        case spaceID = "space_id"
        case legacySpaceID = "native_workspace_id"
        case virtualWorkspaceNumber = "virtual_workspace_number"
        case focusedWindowID = "focused_window_id"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        displayID = try container.decodeIfPresent(UInt32.self, forKey: .displayID)
        spaceID = try container.decodeIfPresent(UInt64.self, forKey: .spaceID)
            ?? container.decodeIfPresent(UInt64.self, forKey: .legacySpaceID)
        virtualWorkspaceNumber = try container.decodeIfPresent(
            UInt32.self, forKey: .virtualWorkspaceNumber)
        focusedWindowID = try container.decodeIfPresent(Int32.self, forKey: .focusedWindowID)
    }
}

struct DisplayState: Decodable, Equatable {
    let displayID: UInt32
    let active: Bool
    let visibleSpaceID: UInt64?
    let virtualWorkspaceNumber: UInt32?

    private enum CodingKeys: String, CodingKey {
        case displayID = "display_id"
        case active
        case visibleSpaceID = "visible_space_id"
        case legacySpaceID = "native_workspace_id"
        case virtualWorkspaceNumber = "virtual_workspace_number"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        displayID = try container.decode(UInt32.self, forKey: .displayID)
        active = try container.decode(Bool.self, forKey: .active)
        visibleSpaceID = try container.decodeIfPresent(UInt64.self, forKey: .visibleSpaceID)
            ?? container.decodeIfPresent(UInt64.self, forKey: .legacySpaceID)
        virtualWorkspaceNumber = try container.decodeIfPresent(
            UInt32.self, forKey: .virtualWorkspaceNumber)
    }
}

struct SpaceState: Decodable, Equatable {
    let spaceID: UInt64
    let displayID: UInt32
    let ordinal: UInt32
    let kind: String
    let visible: Bool
    let focused: Bool
    let windows: [WindowState]

    var number: UInt32 { ordinal + 1 }
    var nativeWorkspaceID: UInt64 { spaceID }
    var selected: Bool { visible }
    var active: Bool { focused }

    private enum CodingKeys: String, CodingKey {
        case spaceID = "space_id"
        case displayID = "display_id"
        case ordinal, kind, visible, focused, windows
        case legacyNumber = "number"
        case legacySpaceID = "native_workspace_id"
        case legacySelected = "selected"
        case legacyActive = "active"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        if let id = try container.decodeIfPresent(UInt64.self, forKey: .spaceID) {
            spaceID = id
            displayID = try container.decode(UInt32.self, forKey: .displayID)
            ordinal = try container.decode(UInt32.self, forKey: .ordinal)
            kind = try container.decode(String.self, forKey: .kind)
            visible = try container.decode(Bool.self, forKey: .visible)
            focused = try container.decode(Bool.self, forKey: .focused)
        } else {
            spaceID = try container.decode(UInt64.self, forKey: .legacySpaceID)
            displayID = try container.decodeIfPresent(UInt32.self, forKey: .displayID) ?? 0
            ordinal = try container.decode(UInt32.self, forKey: .legacyNumber) - 1
            kind = "user"
            focused = try container.decode(Bool.self, forKey: .legacyActive)
            visible = try container.decodeIfPresent(Bool.self, forKey: .legacySelected) ?? focused
        }
        windows = try container.decode([WindowState].self, forKey: .windows)
    }
}

struct WindowState: Decodable, Equatable {
    let windowID: Int32
    let bundleID: String
    let appName: String
    let title: String
    let focused: Bool
    let floating: Bool
    let displayID: UInt32?
    let visible: Bool

    private enum CodingKeys: String, CodingKey {
        case windowID = "window_id"
        case bundleID = "bundle_id"
        case appName = "app_name"
        case title, focused, floating
        case displayID = "display_id"
        case visible
    }
}

extension SpaceState {
    var displayOrderedWindows: [WindowState] {
        windows.filter { !$0.floating } + windows.filter(\.floating)
    }
}
