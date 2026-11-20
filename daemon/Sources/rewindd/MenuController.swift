import AppKit
import Foundation
import RewindKit

/// The menu bar item is the only always-visible part of Rewind, so it lives in
/// the daemon: GPUI has no status-item API, and a separate process would need a
/// second run loop just to draw an icon.
final class MenuController: NSObject, NSMenuDelegate {
    private let recorder: Recorder
    private let statusItem: NSStatusItem
    private let menu = NSMenu()

    private var snapshot: State?
    private var cachedDisplays: [DisplayInfo] = []
    private var cachedApplications: [ApplicationInfo] = []
    private var refreshTask: Task<Void, Never>?

    init(recorder: Recorder) {
        self.recorder = recorder
        self.statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        super.init()
    }

    func install() {
        if let button = statusItem.button {
            button.image = NSImage(
                systemSymbolName: "arrow.uturn.backward.circle",
                accessibilityDescription: "Rewind"
            )
            button.image?.isTemplate = true
        }
        menu.delegate = self
        menu.autoenablesItems = false
        statusItem.menu = menu

        reloadSources()
        refreshTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.refresh()
                try? await Task.sleep(for: .seconds(1))
            }
        }
    }

    // MARK: - Menu

    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()

        menu.addItem(statusRow())
        menu.addItem(.separator())

        let toggle = NSMenuItem(
            title: snapshot?.recording == true ? "Stop Recording" : "Start Recording",
            action: #selector(toggleRecording),
            keyEquivalent: ""
        )
        toggle.target = self
        menu.addItem(toggle)

        menu.addItem(captureMenu())
        menu.addItem(.separator())

        if LoginItem.isSupported {
            let login = NSMenuItem(
                title: "Start at Login",
                action: #selector(toggleLoginItem),
                keyEquivalent: ""
            )
            login.target = self
            login.state = LoginItem.isEnabled ? .on : .off
            menu.addItem(login)
        }

        let quit = NSMenuItem(title: "Quit Rewind", action: #selector(quit), keyEquivalent: "q")
        quit.target = self
        menu.addItem(quit)
    }

    func menuDidClose(_ menu: NSMenu) {
        // Sources change while the menu is closed; reload so the next open is fresh.
        reloadSources()
    }

    private func statusRow() -> NSMenuItem {
        let title: String
        if let state = snapshot {
            if !state.recording, let error = state.lastError {
                title = "Capture stopped — \(error)"
            } else if state.recording {
                title = String(
                    format: "Buffering %.0fs of %ds · %d×%d@%d",
                    state.availableSeconds,
                    Int(state.bufferSeconds),
                    state.width,
                    state.height,
                    state.config.fps
                )
            } else {
                title = "Not recording"
            }
        } else {
            title = "Starting…"
        }

        let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        item.isEnabled = false
        return item
    }

    private func captureMenu() -> NSMenuItem {
        let item = NSMenuItem(title: "Capture", action: nil, keyEquivalent: "")
        let submenu = NSMenu()
        submenu.autoenablesItems = false

        let config = snapshot?.config
        let wholeScreen = config?.applicationBundleID == nil

        submenu.addItem(header("Entire Screen"))
        for display in cachedDisplays {
            let entry = NSMenuItem(title: display.name, action: #selector(selectDisplay), keyEquivalent: "")
            entry.target = self
            entry.representedObject = display.index
            entry.state = (wholeScreen && config?.displayIndex == display.index) ? .on : .off
            submenu.addItem(entry)
        }

        if !cachedApplications.isEmpty {
            submenu.addItem(.separator())
            submenu.addItem(header("One Application"))
            for application in cachedApplications.prefix(20) {
                let entry = NSMenuItem(
                    title: application.name,
                    action: #selector(selectApplication),
                    keyEquivalent: ""
                )
                entry.target = self
                entry.representedObject = application.bundleIdentifier
                entry.state = config?.applicationBundleID == application.bundleIdentifier ? .on : .off
                submenu.addItem(entry)
            }
        }

        item.submenu = submenu
        return item
    }

    private func header(_ title: String) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        item.isEnabled = false
        return item
    }

    // MARK: - Actions

    @objc func toggleRecording() {
        let shouldStop = snapshot?.recording == true
        Task { [weak self] in
            guard let self else { return }
            let state = shouldStop
                ? await self.recorder.stop()
                : await self.recorder.start()
            DispatchQueue.main.async { self.snapshot = state }
        }
    }

    @objc func selectDisplay(_ sender: NSMenuItem) {
        guard let index = sender.representedObject as? Int, var config = snapshot?.config else {
            return
        }
        config.displayIndex = index
        config.applicationBundleID = nil
        apply(config)
    }

    @objc func selectApplication(_ sender: NSMenuItem) {
        guard
            let bundleID = sender.representedObject as? String,
            var config = snapshot?.config
        else { return }
        config.applicationBundleID = bundleID
        apply(config)
    }

    @objc func toggleLoginItem() {
        do {
            try LoginItem.set(!LoginItem.isEnabled)
        } catch {
            present(error: error)
        }
    }

    @objc func quit() {
        NSApp.terminate(nil)
    }

    private func apply(_ config: RecorderConfig) {
        Task { [weak self] in
            guard let self else { return }
            let state = await self.recorder.apply(config)
            DispatchQueue.main.async { self.snapshot = state }
        }
    }

    private func present(error: Error) {
        let alert = NSAlert()
        alert.messageText = "Rewind"
        alert.informativeText = "\(error)"
        alert.runModal()
    }

    // MARK: - Refresh

    private func refresh() async {
        let state = await recorder.state()
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            self.snapshot = state
            self.updateStatusIcon(state)
        }
    }

    private func updateStatusIcon(_ state: State) {
        guard let button = statusItem.button else { return }
        let symbol = state.recording ? "record.circle" : "pause.circle"
        button.image = NSImage(systemSymbolName: symbol, accessibilityDescription: "Rewind")
        button.image?.isTemplate = true
        button.toolTip = state.recording
            ? String(format: "Rewind — %.0fs buffered", state.availableSeconds)
            : "Rewind — not recording"
    }

    private func reloadSources() {
        Task { [weak self] in
            guard let self else { return }
            let displays = (try? await self.recorder.displays()) ?? []
            let applications = (try? await self.recorder.applications()) ?? []
            DispatchQueue.main.async {
                self.cachedDisplays = displays
                self.cachedApplications = applications
            }
        }
    }
}
