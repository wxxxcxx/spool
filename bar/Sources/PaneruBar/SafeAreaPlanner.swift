import AppKit
import CoreGraphics

struct ScreenGeometry: Equatable {
    let frame: CGRect
    let visibleFrame: CGRect
    let safeTopInset: CGFloat
    let auxiliaryLeft: CGRect?
    let auxiliaryRight: CGRect?
    let trayFrames: [CGRect]
}

struct BarPanelPlan: Equatable {
    let frame: CGRect
}

enum SafeAreaPlanner {
    private static let edgePadding: CGFloat = 4

    static func plan(
        contentWidth: CGFloat,
        minimumWidth: CGFloat,
        geometry: ScreenGeometry,
        barHeight: CGFloat
    ) -> BarPanelPlan? {
        let region = rightMenuRegion(in: geometry).insetBy(dx: edgePadding, dy: 0)
        guard region.width >= minimumWidth else { return nil }

        let trayStart = geometry.trayFrames
            .filter { $0.maxX > region.minX && $0.minX < region.maxX }
            .map(\.minX)
            .min()
        let availableMaxX = trayStart.map { $0 - edgePadding } ?? region.maxX
        let available = CGRect(
            x: region.minX,
            y: region.minY,
            width: max(0, availableMaxX - region.minX),
            height: region.height
        )
        guard available.width >= minimumWidth else { return nil }

        let panelHeight = max(1, barHeight)
        let width = min(max(minimumWidth, contentWidth), available.width)
        return BarPanelPlan(frame: CGRect(
            x: available.maxX - width,
            y: available.midY - panelHeight / 2,
            width: width,
            height: panelHeight
        ))
    }

    static func rightMenuRegion(in geometry: ScreenGeometry) -> CGRect {
        if let right = geometry.auxiliaryRight?.intersection(geometry.frame),
           !right.isNull,
           right.width > 0,
           right.height > 0
        {
            return right
        }

        let screen = geometry.frame
        let menuBarHeight = max(
            screen.maxY - geometry.visibleFrame.maxY,
            geometry.safeTopInset,
            NSStatusBar.system.thickness
        )
        return CGRect(
            x: screen.midX,
            y: screen.maxY - menuBarHeight,
            width: screen.width / 2,
            height: menuBarHeight
        )
    }
}

enum MenuBarOccupancy {
    static func trayFrames(on screen: NSScreen) -> [CGRect] {
        guard let displayID = screen.displayID else { return [] }
        let options: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
        guard let rows = CGWindowListCopyWindowInfo(options, kCGNullWindowID)
            as? [[String: Any]]
        else { return [] }

        let statusLevel = Int(CGWindowLevelForKey(.statusWindow))
        let ownPID = Int(ProcessInfo.processInfo.processIdentifier)
        return rows.compactMap { row in
            guard (row[kCGWindowOwnerPID as String] as? Int) != ownPID,
                  !((row[kCGWindowOwnerName as String] as? String) ?? "")
                      .hasPrefix("PaneruBar"),
                  let layer = row[kCGWindowLayer as String] as? Int,
                  layer >= statusLevel,
                  let bounds = row[kCGWindowBounds as String] as? [String: Any],
                  let cgFrame = CGRect(dictionaryRepresentation: bounds as CFDictionary),
                  let frame = appKitFrame(cgFrame, displayID: displayID, screen: screen),
                  frame.width > 0,
                  frame.width < screen.frame.width / 2,
                  frame.height <= menuBarFrame(on: screen).height * 1.5,
                  frame.intersects(menuBarFrame(on: screen))
            else { return nil }
            return frame
        }
    }

    private static func menuBarFrame(on screen: NSScreen) -> CGRect {
        let height = max(
            screen.frame.maxY - screen.visibleFrame.maxY,
            screen.safeAreaInsets.top,
            NSStatusBar.system.thickness
        )
        return CGRect(
            x: screen.frame.minX,
            y: screen.frame.maxY - height,
            width: screen.frame.width,
            height: height
        )
    }

    private static func appKitFrame(
        _ cgFrame: CGRect,
        displayID: CGDirectDisplayID,
        screen: NSScreen
    ) -> CGRect? {
        let displayBounds = CGDisplayBounds(displayID)
        guard cgFrame.intersects(displayBounds) else { return nil }
        return CGRect(
            x: screen.frame.minX + cgFrame.minX - displayBounds.minX,
            y: screen.frame.maxY - (cgFrame.minY - displayBounds.minY) - cgFrame.height,
            width: cgFrame.width,
            height: cgFrame.height
        )
    }
}

extension NSScreen {
    var displayID: CGDirectDisplayID? {
        (deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value
    }
}
