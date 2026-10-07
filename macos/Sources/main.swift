import AppKit
import WebKit
import ServiceManagement
import Security

// Kept alive by the setup sheet's completion handler; no password is written to disk.
final class PasswordControls: NSObject, NSTextFieldDelegate {
    let password = NSSecureTextField()
    let confirmation = NSSecureTextField()
    let preview = NSTextField(string: "")
    let feedback = NSTextField(wrappingLabelWithString: "Choose your own password or generate one.")
    let copyButton = NSButton(title: "Copy", target: nil, action: nil)
    let generateButton = NSButton(title: "Generate a password", target: nil, action: nil)

    override init() {
        super.init()
        password.placeholderString = "Password (at least 12 characters)"
        confirmation.placeholderString = "Confirm password"
        password.delegate = self; confirmation.delegate = self
        preview.isEditable = false; preview.isSelectable = true
        preview.font = .monospacedSystemFont(ofSize: 13, weight: .regular)
        preview.placeholderString = "Your generated password will appear here"
        preview.setAccessibilityLabel("Generated password")
        feedback.font = .systemFont(ofSize: 11)
        feedback.textColor = .secondaryLabelColor
        copyButton.target = self; copyButton.action = #selector(copyPassword); copyButton.isEnabled = false
        generateButton.target = self; generateButton.action = #selector(generate)
    }
    @objc func generate() {
        var bytes = [UInt8](repeating: 0, count: 18)
        guard SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes) == errSecSuccess else {
            feedback.stringValue = "Generation unavailable. Try again or choose your own password."
            return
        }
        let value = Data(bytes).base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_")
        password.stringValue = value; confirmation.stringValue = value; preview.stringValue = value
        copyButton.isEnabled = true
        feedback.stringValue = "24 random characters. Save this password in your password manager before continuing."
    }
    @objc func copyPassword() {
        guard !preview.stringValue.isEmpty else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(preview.stringValue, forType: .string)
        feedback.stringValue = "Password copied. Save it in your password manager."
    }
    func controlTextDidChange(_ notification: Notification) {
        preview.stringValue = ""; copyButton.isEnabled = false
        feedback.stringValue = "Custom password: check that the confirmation matches."
    }
    func clear() {
        password.stringValue = ""; confirmation.stringValue = ""; preview.stringValue = ""
        copyButton.isEnabled = false
    }
}

// A small native shell: the Rust server and web console remain the single implementation.
final class AppDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate, WKNavigationDelegate, WKUIDelegate, WKScriptMessageHandler {
    let fm = FileManager.default
    let smoke = CommandLine.arguments.contains("--smoke-test")
    lazy var data: URL = {
        if smoke { return fm.temporaryDirectory.appendingPathComponent("customremote-app-qa-\(UUID().uuidString)") }
        return fm.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0].appendingPathComponent("PocketRelay")
    }()
    var resources: URL { Bundle.main.resourceURL! }
    var tools: URL { resources.appendingPathComponent("bin") }
    var port: Int { smoke ? 18789 : 8788 }
    var base: URL { URL(string: "http://localhost:\(port)")! }
    var server: Process?
    var parentPipe: Pipe?
    var logHandle: FileHandle?
    var window: NSWindow!
    var web: WKWebView!
    var statusItem: NSStatusItem!
    var stateItem: NSMenuItem!
    var loginItem: NSMenuItem!
    var timer: Timer?
    var ready = false
    var quitting = false
    var setupRunning = false
    var autoLoginOnce = false
    var smokePassed = false
    var smokeChecking = false
    var smokePassword = ""
    var launchTime = Date()

    func applicationDidFinishLaunching(_ notification: Notification) {
        // Do not start a second server if the app was launched twice by different paths.
        if !smoke, let id = Bundle.main.bundleIdentifier,
           let existing = NSRunningApplication.runningApplications(withBundleIdentifier: id).first(where: { $0.processIdentifier != ProcessInfo.processInfo.processIdentifier }) {
            existing.activate(options: [.activateAllWindows])
            NSApp.terminate(nil)
            return
        }
        do {
            try fm.createDirectory(at: data, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
            try fm.setAttributes([.posixPermissions: 0o700], ofItemAtPath: data.path)
            try fm.createDirectory(at: data.appendingPathComponent("home"), withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        } catch { fatalMessage(error.localizedDescription); return }
        buildMenu()
        buildWindow()
        if smoke {
            let controls = PasswordControls()
            controls.generate()
            smokePassword = controls.password.stringValue
            guard smokePassword.count == 24, controls.confirmation.stringValue == smokePassword,
                  controls.preview.stringValue == smokePassword else { finishSmoke(false); return }
            configure(username: "qa", password: smokePassword, onlyMissing: true) { ok in
                if ok { self.startServer() } else { self.finishSmoke(false) }
            }
        } else if !fm.fileExists(atPath: data.appendingPathComponent("admin.json").path) {
            showWindow()
            setupAccount(first: true)
        } else {
            startServer()
            showWindow()
        }
    }
    func environment() -> [String: String] {
        // GUI launches don't inherit shell PATH. Never import provider credentials from the host.
        var env: [String: String] = [
            "PATH": tools.path + ":/usr/bin:/bin:/usr/sbin:/sbin", "HOME": data.appendingPathComponent("home").path,
            "LANG": "en_US.UTF-8", "REMOTE_DATA": data.path, "REMOTE_STATIC": resources.appendingPathComponent("static").path,
            "REMOTE_HOST": "127.0.0.1", "REMOTE_PORT": String(port), "REMOTE_ENABLE_SESSIONS": "0",
            "REMOTE_SYSTEM_ACCOUNTS": "", "REMOTE_V1_ALLOW_MASTER": "0", "DISABLE_AUTOUPDATER": "1",
            "CLAUDE_BIN": tools.appendingPathComponent("claude").path,
            "CODEX_BIN": tools.appendingPathComponent("codex").path,
            "ANTIGRAVITY_BIN": tools.appendingPathComponent("antigravity").path
        ]
        for key in ["TMPDIR", "USER", "LOGNAME"] { env[key] = ProcessInfo.processInfo.environment[key] }
        return env
    }
    func buildMenu() {
        let main = NSMenu()
        let appItem = NSMenuItem()
        let appMenu = NSMenu()
        appMenu.addItem(withTitle: "Open Pocket Relay", action: #selector(showWindow), keyEquivalent: "0").target = self
        appMenu.addItem(.separator())
        appMenu.addItem(withTitle: "Quit Pocket Relay and stop the API", action: #selector(quit), keyEquivalent: "q").target = self
        appItem.submenu = appMenu; main.addItem(appItem)
        let editItem = NSMenuItem(); let edit = NSMenu(title: "Edit")
        for (title, action, key) in [("Undo", "undo:", "z"), ("Cut", "cut:", "x"), ("Copy", "copy:", "c"), ("Paste", "paste:", "v"), ("Select all", "selectAll:", "a")] {
            edit.addItem(withTitle: title, action: Selector(action), keyEquivalent: key)
        }
        editItem.submenu = edit; main.addItem(editItem); NSApp.mainMenu = main
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        statusItem.button?.image = NSImage(systemSymbolName: "terminal", accessibilityDescription: "Pocket Relay")
        let menu = NSMenu()
        stateItem = NSMenuItem(title: "Preparing…", action: nil, keyEquivalent: ""); menu.addItem(stateItem)
        for (title, action) in [("Open console", #selector(showWindow)), ("Copy API address", #selector(copyAPI)), ("Open in browser", #selector(openBrowser)), ("Restart service", #selector(restart))] {
            let item = menu.addItem(withTitle: title, action: action, keyEquivalent: ""); item.target = self
        }
        menu.addItem(.separator())
        loginItem = menu.addItem(withTitle: "Launch at login", action: #selector(toggleLogin), keyEquivalent: "")
        loginItem.target = self; updateLoginItem()
        menu.addItem(withTitle: "Change password…", action: #selector(resetPassword), keyEquivalent: "").target = self
        menu.addItem(withTitle: "Show data", action: #selector(showData), keyEquivalent: "").target = self
        menu.addItem(withTitle: "Show log", action: #selector(showLog), keyEquivalent: "").target = self
        menu.addItem(.separator())
        menu.addItem(withTitle: "Quit and stop the API", action: #selector(quit), keyEquivalent: "q").target = self
        statusItem.menu = menu
    }
    func buildWindow() {
        let configuration = WKWebViewConfiguration()
        if smoke { configuration.websiteDataStore = .nonPersistent() }
        configuration.userContentController.add(self, name: "customremote")
        web = WKWebView(frame: .zero, configuration: configuration)
        web.navigationDelegate = self; web.uiDelegate = self
        window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1180, height: 820), styleMask: [.titled, .closable, .miniaturizable, .resizable], backing: .buffered, defer: false)
        window.title = "Pocket Relay — Local API: \(port)"
        window.minSize = NSSize(width: 760, height: 560)
        window.contentView = web; window.delegate = self; window.isReleasedWhenClosed = false; window.center()
        showStatus("Welcome to Pocket Relay", "Connect your AI accounts and use them from your applications. The service keeps running when you close this window.")
    }
    func showStatus(_ title: String, _ message: String) {
        // Strings here are application-owned; process errors are displayed in native alerts.
        web.loadHTMLString("<html><meta name='color-scheme' content='light dark'><body style='font:17px -apple-system;padding:12%;line-height:1.6'><h1>\(title)</h1><p>\(message)</p><p>Use the menu bar to open the console, restart the service or quit.</p></body></html>", baseURL: nil)
    }
    @objc func showWindow() { window?.makeKeyAndOrderFront(nil); NSApp.activate(ignoringOtherApps: true) }
    @objc func copyAPI() { NSPasteboard.general.clearContents(); NSPasteboard.general.setString(base.absoluteString + "/v1", forType: .string) }
    @objc func openBrowser() { if ready { NSWorkspace.shared.open(base.appendingPathComponent("admin")) } }
    @objc func showData() { NSWorkspace.shared.open(data) }
    @objc func showLog() { NSWorkspace.shared.activateFileViewerSelecting([data.appendingPathComponent("server.log")]) }
    @objc func quit() { NSApp.terminate(nil) }
    func updateLoginItem() { loginItem?.state = SMAppService.mainApp.status == .enabled ? .on : .off }
    @objc func toggleLogin() {
        do {
            if SMAppService.mainApp.status == .enabled { try SMAppService.mainApp.unregister() }
            else { try SMAppService.mainApp.register() }
            if SMAppService.mainApp.status == .requiresApproval { SMAppService.openSystemSettingsLoginItems() }
        } catch { alert("Login", error.localizedDescription) }
        updateLoginItem()
    }
    func alert(_ title: String, _ message: String) {
        let a = NSAlert(); a.messageText = title; a.informativeText = message; a.addButton(withTitle: "OK")
        showWindow(); a.beginSheetModal(for: window)
    }
    func fatalMessage(_ message: String) {
        if smoke { fputs(message + "\n", stderr); finishSmoke(false); return }
        alert("The service did not start", message)
    }
    @objc func resetPassword() { setupAccount(first: false) }
    func setupAccount(first: Bool, validationError: String? = nil) {
        guard !setupRunning else { return }
        setupRunning = true; showWindow()
        let a = NSAlert(); a.messageText = first ? "Set up access to Pocket Relay" : "Change administrator access"
        a.informativeText = validationError ?? "Choose a password with at least 12 characters. Your accounts and keys stay on this Mac."
        a.addButton(withTitle: first ? "Create and start" : "Save"); a.addButton(withTitle: "Cancel")
        let stack = NSStackView(); stack.orientation = .vertical; stack.alignment = .leading; stack.spacing = 8
        let username = NSTextField(string: "admin"); username.placeholderString = "Username"
        let passwordControls = PasswordControls()
        let password = passwordControls.password
        let confirm = passwordControls.confirmation
        for (label, field) in [("Username", username), ("Password", password), ("Confirmation", confirm)] {
            field.setAccessibilityLabel(label)
            stack.addArrangedSubview(NSTextField(labelWithString: label)); stack.addArrangedSubview(field)
            field.widthAnchor.constraint(equalToConstant: 360).isActive = true
        }
        let buttons = NSStackView(views: [passwordControls.generateButton, passwordControls.copyButton])
        buttons.spacing = 8
        stack.addArrangedSubview(buttons)
        stack.addArrangedSubview(passwordControls.preview)
        stack.addArrangedSubview(passwordControls.feedback)
        passwordControls.preview.widthAnchor.constraint(equalToConstant: 360).isActive = true
        passwordControls.feedback.widthAnchor.constraint(equalToConstant: 360).isActive = true
        stack.frame = NSRect(x: 0, y: 0, width: 360, height: 300); a.accessoryView = stack
        a.beginSheetModal(for: window) { response in
            self.setupRunning = false
            guard response == .alertFirstButtonReturn else { passwordControls.clear(); if first { NSApp.terminate(nil) }; return }
            guard password.stringValue == confirm.stringValue, (12...256).contains(password.stringValue.count), !username.stringValue.trimmingCharacters(in: .whitespaces).isEmpty else {
                self.setupAccount(first: first, validationError: "Passwords must match and contain 12 to 256 characters. The username must not be empty."); return
            }
            self.configure(username: username.stringValue, password: password.stringValue, onlyMissing: first) { ok in
                passwordControls.clear()
                if ok {
                    if self.server?.isRunning == true { self.web.load(URLRequest(url: self.base.appendingPathComponent("admin"))) }
                    else { self.autoLoginOnce = first; self.startServer() }
                }
            }
        }
    }
    func configure(username: String, password: String, onlyMissing: Bool, done: @escaping (Bool) -> Void) {
        let p = Process(); p.executableURL = tools.appendingPathComponent("customremote")
        p.arguments = ["setup", "--json-stdin"] + (onlyMissing ? ["--if-missing"] : [])
        p.environment = environment()
        let input = Pipe(); let output = Pipe(); p.standardInput = input; p.standardOutput = output; p.standardError = output
        p.terminationHandler = { process in
            let error = String(data: output.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
            DispatchQueue.main.async {
                if process.terminationStatus != 0 { self.fatalMessage(error) }
                done(process.terminationStatus == 0)
            }
        }
        do {
            try p.run()
            let bytes = try JSONSerialization.data(withJSONObject: ["username": username, "password": password])
            try input.fileHandleForWriting.write(contentsOf: bytes); try input.fileHandleForWriting.close()
        } catch { fatalMessage(error.localizedDescription); done(false) }
    }
    func startServer() {
        guard server?.isRunning != true else { return }
        ready = false; stateItem.title = "Starting…"; launchTime = Date()
        showStatus("Starting the service…", "Your API will be available at <b>http://localhost:\(port)/v1</b>.")
        let p = Process(); p.executableURL = tools.appendingPathComponent("customremote")
        p.arguments = ["serve", "--exit-on-stdin-close"]; p.environment = environment(); p.currentDirectoryURL = data
        let pipe = Pipe(); p.standardInput = pipe; parentPipe = pipe
        do {
            let log = data.appendingPathComponent("server.log")
            // Bound logs across launches, preserve one previous file for diagnostics.
            if let size = try? fm.attributesOfItem(atPath: log.path)[.size] as? NSNumber, size.intValue > 2_000_000 {
                let previous = data.appendingPathComponent("server.previous.log"); try? fm.removeItem(at: previous); try fm.moveItem(at: log, to: previous)
            }
            if !fm.fileExists(atPath: log.path) { fm.createFile(atPath: log.path, contents: nil, attributes: [.posixPermissions: 0o600]) }
            logHandle = try FileHandle(forWritingTo: log); try logHandle?.seekToEnd()
            p.standardOutput = logHandle; p.standardError = logHandle
            p.terminationHandler = { process in DispatchQueue.main.async {
                guard self.server === process else { return }
                self.ready = false; self.timer?.invalidate(); self.stateItem.title = "Service stopped"
                if self.quitting { NSApp.reply(toApplicationShouldTerminate: true) }
                else {
                    self.showStatus("The service is stopped", "Restart it from the menu bar. If another application is using port \(self.port), free that port first.")
                    if self.smoke { self.finishSmoke(false) }
                }
            } }
            server = p; try p.run()
            // The parent only retains the writer. Closing it also stops Rust after an app crash.
            try pipe.fileHandleForReading.close()
            timer?.invalidate(); timer = Timer.scheduledTimer(withTimeInterval: 0.35, repeats: true) { _ in self.checkReady() }
        } catch { server = nil; fatalMessage(error.localizedDescription) }
    }
    func checkReady() {
        guard let process = server, process.isRunning else { return }
        if Date().timeIntervalSince(launchTime) > 30 {
            timer?.invalidate(); try? parentPipe?.fileHandleForWriting.close()
            fatalMessage("Startup is taking more than 30 seconds. Open the log from the menu bar."); return
        }
        // Authenticate the probe with this instance's token: never attach to a different local server.
        guard let token = try? String(contentsOf: data.appendingPathComponent("token"), encoding: .utf8) else { return }
        var request = URLRequest(url: base.appendingPathComponent("admin/auth/token")); request.httpMethod = "POST"; request.timeoutInterval = 1
        request.setValue("1", forHTTPHeaderField: "X-Admin"); request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try? JSONSerialization.data(withJSONObject: ["token": token])
        let config = URLSessionConfiguration.ephemeral; config.httpCookieStorage = nil
        let session = URLSession(configuration: config)
        session.dataTask(with: request) { _, response, _ in
            session.finishTasksAndInvalidate()
            DispatchQueue.main.async {
                guard self.server === process, process.isRunning, !self.ready, !self.quitting,
                      (response as? HTTPURLResponse)?.statusCode == 200 else { return }
                self.ready = true; self.timer?.invalidate(); self.stateItem.title = "API active · localhost:\(self.port)"
                if self.autoLoginOnce, let response = response as? HTTPURLResponse,
                   let fields = response.allHeaderFields as? [String: String],
                   let cookie = HTTPCookie.cookies(withResponseHeaderFields: fields, for: self.base).first {
                    self.autoLoginOnce = false
                    self.web.configuration.websiteDataStore.httpCookieStore.setCookie(cookie) {
                        self.web.load(URLRequest(url: self.base.appendingPathComponent("admin")))
                    }
                } else {
                    self.web.load(URLRequest(url: self.base.appendingPathComponent("admin")))
                }
            }
        }.resume()
    }
    @objc func restart() {
        guard !quitting else { return }
        if !fm.fileExists(atPath: data.appendingPathComponent("admin.json").path) { setupAccount(first: true); return }
        timer?.invalidate()
        guard let process = server, process.isRunning else { startServer(); return }
        stateItem.title = "Restarting…"; ready = false
        process.terminationHandler = { _ in DispatchQueue.main.async { self.server = nil; self.startServer() } }
        try? parentPipe?.fileHandleForWriting.close()
    }
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool { showWindow(); return true }
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        quitting = true; timer?.invalidate()
        guard let process = server, process.isRunning else { cleanup(); return .terminateNow }
        process.terminationHandler = { _ in DispatchQueue.main.async { self.cleanup(); NSApp.reply(toApplicationShouldTerminate: true) } }
        try? parentPipe?.fileHandleForWriting.close()
        DispatchQueue.main.asyncAfter(deadline: .now() + 8) {
            if process.isRunning { process.terminate() }
        }
        return .terminateLater
    }
    func cleanup() {
        try? logHandle?.close()
        if smoke { try? fm.removeItem(at: data) }
    }
    func finishSmoke(_ ok: Bool) {
        smokePassed = ok
        print(ok ? "PASS: native setup, owned server, WebKit login/cookie/key/API" : "FAIL: macOS app smoke test")
        if ok && CommandLine.arguments.contains("--show-test-window") {
            web.load(URLRequest(url: base.appendingPathComponent("admin")))
            showWindow()
            return
        }
        NSApp.terminate(nil)
    }
    func isLocal(_ url: URL?) -> Bool {
        guard let url else { return false }
        return url.scheme == "http" && url.host == "localhost" && url.port == port
    }
    func webView(_ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction, decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        guard let url = navigationAction.request.url else { decisionHandler(.cancel); return }
        if isLocal(url) || url.absoluteString == "about:blank" { decisionHandler(.allow); return }
        // No native bridge on remote pages. OAuth always goes to the user's browser.
        if navigationAction.navigationType == .linkActivated && ["https", "http"].contains(url.scheme ?? "") { NSWorkspace.shared.open(url) }
        decisionHandler(.cancel)
    }
    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration, for navigationAction: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        if let url = navigationAction.request.url, ["https", "http"].contains(url.scheme ?? "") { NSWorkspace.shared.open(url) }
        return nil
    }
    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        if smoke && ready && isLocal(webView.url) && !smokeChecking {
            smokeChecking = true
            webView.callAsyncJavaScript("""
                const headers = { 'Content-Type': 'application/json', 'X-Admin': '1' };
                const login = await fetch('/admin/auth/password', { method: 'POST', headers,
                    body: JSON.stringify({username:'qa', password:testPassword}) });
                if (!login.ok) throw Error('WebKit password login failed');
                const state = await fetch('/admin/api/state');
                if (!state.ok) throw Error('WebKit session cookie failed');
                const created = await fetch('/admin/api/keys', { method: 'POST', headers, body: JSON.stringify({name:'macOS smoke'}) });
                const key = await created.json();
                if (!created.ok || !key.key) throw Error('WebKit key creation failed');
                const models = await fetch('/v1/models', {headers: {Authorization:'Bearer ' + key.key}});
                return models.ok && Boolean(document.querySelector('#password-form') && window.webkit.messageHandlers.customremote);
                """, arguments: ["testPassword": smokePassword], in: nil, in: .page) { result in
                switch result {
                case .success(let value): self.finishSmoke((value as? Bool) == true)
                case .failure(let error): fputs(error.localizedDescription + "\n", stderr); self.finishSmoke(false)
                }
            }
        }
    }
    func webView(_ webView: WKWebView, runJavaScriptConfirmPanelWithMessage message: String, initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping (Bool) -> Void) {
        guard isLocal(frame.request.url) else { completionHandler(false); return }
        let dialog = NSAlert(); dialog.messageText = "Pocket Relay"; dialog.informativeText = message
        dialog.addButton(withTitle: "Confirm"); dialog.addButton(withTitle: "Cancel")
        dialog.beginSheetModal(for: window) { completionHandler($0 == .alertFirstButtonReturn) }
    }
    func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
        guard ready, message.frameInfo.isMainFrame, isLocal(message.frameInfo.request.url),
              let body = message.body as? [String: String], let action = body["action"] else { return }
        if action == "resetPassword" { resetPassword(); return }
        if action == "copy", let text = body["text"], text.utf8.count <= 1_000_000 {
            NSPasteboard.general.clearContents(); NSPasteboard.general.setString(text, forType: .string); return
        }
        if action == "claudeToken" { openLoginTerminal(provider: "claude", account: nil) }
        if action == "googleLogin", let id = body["account"] {
            // Derive all executable paths locally; never execute shell text supplied by the page.
            guard let bytes = try? Data(contentsOf: data.appendingPathComponent("accounts.json")),
                  let accounts = try? JSONSerialization.jsonObject(with: bytes) as? [[String: Any]],
                  accounts.contains(where: { ($0["id"] as? String) == id && ($0["provider"] as? String) == "antigravity" }),
                  id.range(of: "^[a-zA-Z0-9_-]{1,80}$", options: .regularExpression) != nil else { return }
            openLoginTerminal(provider: "antigravity", account: id)
        }
    }
    func shellQuote(_ string: String) -> String { "'" + string.replacingOccurrences(of: "'", with: "'\\''") + "'" }
    func openLoginTerminal(provider: String, account: String?) {
        let home = account.map { data.appendingPathComponent("accounts/\($0)/antigravity") } ?? data.appendingPathComponent("home")
        let script = data.appendingPathComponent(provider + "-login.command")
        let command = "#!/bin/sh\nunset ANTHROPIC_API_KEY OPENAI_API_KEY GEMINI_API_KEY GOOGLE_API_KEY\nexport HOME=\(shellQuote(home.path))\nexport PATH=\(shellQuote(tools.path + ":/usr/bin:/bin:/usr/sbin:/sbin"))\nexport DISABLE_AUTOUPDATER=1\n\(shellQuote(tools.appendingPathComponent(provider).path))\(provider == "claude" ? " setup-token" : "")\nprintf '\\nReturn to Pocket Relay to finish connecting.\\n'\n"
        do {
            try fm.createDirectory(at: home, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
            try command.write(to: script, atomically: true, encoding: .utf8)
            try fm.setAttributes([.posixPermissions: 0o700], ofItemAtPath: script.path)
            NSWorkspace.shared.open(script)
        } catch { alert("Provider sign-in", error.localizedDescription) }
    }
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.run()
if delegate.smoke { exit(delegate.smokePassed ? 0 : 1) }
