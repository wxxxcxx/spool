import Foundation

struct SpoolStateDocument: Decodable, Equatable {
    let version: UInt32
    let timestamp: UInt64
    let active: ActiveState
    let displays: [DisplayState]
    let virtualWorkspaces: [VirtualWorkspaceState]

    private enum CodingKeys: String, CodingKey {
        case version, timestamp, active, displays
        case virtualWorkspaces = "virtual_workspaces"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        version = try container.decode(UInt32.self, forKey: .version)
        timestamp = try container.decode(UInt64.self, forKey: .timestamp)
        active = try container.decode(ActiveState.self, forKey: .active)
        displays = try container.decodeIfPresent([DisplayState].self, forKey: .displays) ?? []
        virtualWorkspaces = try container.decode([VirtualWorkspaceState].self, forKey: .virtualWorkspaces)
    }

    func display(_ displayID: UInt32) -> DisplayState? {
        displays.first { $0.displayID == displayID }
    }

    func visibleWorkspaces(on display: DisplayState) -> [VirtualWorkspaceState] {
        guard let nativeWorkspaceID = display.nativeWorkspaceID else { return [] }
        return virtualWorkspaces
            .filter {
                $0.displayID == display.displayID &&
                    $0.nativeWorkspaceID == nativeWorkspaceID
            }
            .sorted { $0.number < $1.number }
    }
}

struct ActiveState: Decodable, Equatable {
    let displayID: UInt32?
    let nativeWorkspaceID: UInt64?
    let virtualWorkspaceNumber: UInt32?
    let focusedWindowID: Int32?

    private enum CodingKeys: String, CodingKey {
        case displayID = "display_id"
        case nativeWorkspaceID = "native_workspace_id"
        case virtualWorkspaceNumber = "virtual_workspace_number"
        case focusedWindowID = "focused_window_id"
    }
}

struct DisplayState: Decodable, Equatable {
    let displayID: UInt32
    let active: Bool
    let nativeWorkspaceID: UInt64?
    let virtualWorkspaceNumber: UInt32?

    private enum CodingKeys: String, CodingKey {
        case displayID = "display_id"
        case active
        case nativeWorkspaceID = "native_workspace_id"
        case virtualWorkspaceNumber = "virtual_workspace_number"
    }
}

struct VirtualWorkspaceState: Decodable, Equatable {
    let number: UInt32
    let nativeWorkspaceID: UInt64
    let displayID: UInt32?
    let selected: Bool
    let active: Bool
    let windows: [WindowState]

    private enum CodingKeys: String, CodingKey {
        case number
        case nativeWorkspaceID = "native_workspace_id"
        case displayID = "display_id"
        case selected, active, windows
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        number = try container.decode(UInt32.self, forKey: .number)
        nativeWorkspaceID = try container.decode(UInt64.self, forKey: .nativeWorkspaceID)
        displayID = try container.decodeIfPresent(UInt32.self, forKey: .displayID)
        active = try container.decode(Bool.self, forKey: .active)
        selected = try container.decodeIfPresent(Bool.self, forKey: .selected) ?? active
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

extension VirtualWorkspaceState {
    var displayOrderedWindows: [WindowState] {
        windows.filter { !$0.floating } + windows.filter(\.floating)
    }
}
