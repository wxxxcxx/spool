import Foundation

protocol SpoolControlling: AnyObject {
    func requestRefresh()
    func focus(windowID: Int32)
    func selectWorkspace(displayID: UInt32, number: UInt32)
    func moveWindow(
        windowID: Int32,
        displayID: UInt32,
        workspaceNumber: UInt32,
        follow: Bool
    )
}

final class SpoolClient: SpoolControlling {
    var onState: ((SpoolStateDocument) -> Void)?
    var onError: ((String) -> Void)?

    private let queue = DispatchQueue(label: "com.wxxxcxx.spool-bar.client", qos: .userInitiated)
    private let decoder = JSONDecoder()
    private var subscription: Process?
    private var stopped = false
    private var queryRunning = false
    private var queryPending = false
    private var refreshWorkItem: DispatchWorkItem?
    private var latestState: SpoolStateDocument?

    func start() {
        queue.async { [weak self] in
            guard let self else { return }
            self.stopped = false
            self.refreshNow()
            self.startSubscription()
        }
    }

    func stop() {
        queue.async { [weak self] in
            guard let self else { return }
            self.stopped = true
            self.refreshWorkItem?.cancel()
            self.subscription?.terminationHandler = nil
            self.subscription?.terminate()
            self.subscription = nil
        }
    }

    func requestRefresh() {
        queue.async { [weak self] in self?.scheduleRefresh() }
    }

    func focus(windowID: Int32) {
        send(["action", "window", "focusid", String(windowID)])
    }

    func selectWorkspace(displayID: UInt32, number: UInt32) {
        queue.async { [weak self] in
            guard let self,
                let space = latestState?.spaces.first(where: {
                    $0.displayID == displayID && $0.number == number
                }),
                latestState?.capabilities.focus == true
            else { return }
            self.sendNow(["action", "space", "focus", String(space.spaceID)])
        }
    }

    func moveWindow(
        windowID: Int32,
        displayID: UInt32,
        workspaceNumber: UInt32,
        follow: Bool = true
    ) {
        queue.async { [weak self] in
            guard let self,
                let space = latestState?.spaces.first(where: {
                    $0.displayID == displayID && $0.number == workspaceNumber
                }),
                latestState?.capabilities.moveWindows == true
            else { return }
            self.sendNow([
                "action", "window", "move-to-space", String(windowID),
                String(space.spaceID), follow ? "follow" : "stay",
            ])
        }
    }

    private func send(_ arguments: [String]) {
        queue.async { [weak self] in
            guard let self, !self.stopped else { return }
            self.sendNow(arguments)
        }
    }

    private func sendNow(_ arguments: [String]) {
        guard !stopped else { return }
        let process = makeProcess(arguments)
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        do {
            try process.run()
            process.waitUntilExit()
            if process.terminationStatus != 0 {
                report("Spool action failed: \(arguments.joined(separator: " "))")
            }
        } catch {
            report("Unable to dispatch Spool action: \(error.localizedDescription)")
        }
    }

    private func scheduleRefresh() {
        refreshWorkItem?.cancel()
        let work = DispatchWorkItem { [weak self] in self?.refreshNow() }
        refreshWorkItem = work
        queue.asyncAfter(deadline: .now() + .milliseconds(45), execute: work)
    }

    private func refreshNow() {
        guard !stopped else { return }
        if queryRunning {
            queryPending = true
            return
        }
        queryRunning = true

        let process = makeProcess(["query", "state", "--json"])
        let output = Pipe()
        let errors = Pipe()
        process.standardOutput = output
        process.standardError = errors
        do {
            try process.run()
            let data = output.fileHandleForReading.readDataToEndOfFile()
            let errorData = errors.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            if process.terminationStatus == 0 {
                do {
                    let document = try decoder.decode(SpoolStateDocument.self, from: data)
                    latestState = document
                    DispatchQueue.main.async { [weak self] in self?.onState?(document) }
                } catch {
                    report("Unable to decode Spool state: \(error.localizedDescription)")
                }
            } else {
                let detail = String(data: errorData, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines)
                report(detail?.isEmpty == false ? detail! : "Spool state query failed")
            }
        } catch {
            report("Unable to query Spool: \(error.localizedDescription)")
        }

        queryRunning = false
        if queryPending {
            queryPending = false
            scheduleRefresh()
        }
    }

    private func startSubscription() {
        guard !stopped, subscription == nil else { return }
        let process = makeProcess(["subscribe", "--json"])
        let output = Pipe()
        process.standardOutput = output
        process.standardError = FileHandle.nullDevice
        output.fileHandleForReading.readabilityHandler = { [weak self] handle in
            guard !handle.availableData.isEmpty else { return }
            self?.queue.async { [weak self] in self?.scheduleRefresh() }
        }
        process.terminationHandler = { [weak self, weak output] _ in
            output?.fileHandleForReading.readabilityHandler = nil
            self?.queue.asyncAfter(deadline: .now() + 1) { [weak self] in
                guard let self else { return }
                self.subscription = nil
                self.startSubscription()
            }
        }
        do {
            try process.run()
            subscription = process
        } catch {
            output.fileHandleForReading.readabilityHandler = nil
            report("Unable to subscribe to Spool: \(error.localizedDescription)")
            queue.asyncAfter(deadline: .now() + 1) { [weak self] in self?.startSubscription() }
        }
    }

    private func makeProcess(_ arguments: [String]) -> Process {
        let process = Process()
        if let configured = ProcessInfo.processInfo.environment["SPOOL_CLI"], !configured.isEmpty {
            process.executableURL = URL(fileURLWithPath: configured)
            process.arguments = arguments
        } else {
            process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
            process.arguments = ["spool"] + arguments
        }
        return process
    }

    private func report(_ message: String) {
        NSLog("SpoolBar: %@", message)
        DispatchQueue.main.async { [weak self] in self?.onError?(message) }
    }
}
