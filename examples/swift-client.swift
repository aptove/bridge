import SwiftUI
import Combine

/// Example Swift code for connecting to the ACP bridge from iOS
/// 
/// This demonstrates:
/// 1. Scanning QR code
/// 2. Storing credentials in Keychain
/// 3. Establishing WebSocket connection with Cloudflare Access headers

struct BridgeConnection: Codable {
    let url: String
    let clientId: String
    let clientSecret: String
    let protocol: String
    let version: String
}

class ACPBridgeClient: NSObject, ObservableObject {
    @Published var connectionState: ConnectionState = .disconnected
    
    private var webSocketTask: URLSessionWebSocketTask?
    private var config: BridgeConnection?
    
    enum ConnectionState {
        case disconnected
        case connecting
        case connected
        case error(String)
    }
    
    // MARK: - Configuration from QR Code
    
    func configureFromQRCode(_ qrString: String) throws {
        guard let data = qrString.data(using: .utf8) else {
            throw NSError(domain: "ACPBridge", code: 1, userInfo: [NSLocalizedDescriptionKey: "Invalid QR code format"])
        }
        
        let decoder = JSONDecoder()
        self.config = try decoder.decode(BridgeConnection.self, from: data)
        
        // Store credentials in Keychain
        try saveToKeychain()
    }
    
    // MARK: - WebSocket Connection
    
    func connect() {
        guard let config = config else {
            connectionState = .error("No configuration. Scan QR code first.")
            return
        }
        
        connectionState = .connecting
        
        // Convert https:// to wss://
        let wsURL = config.url.replacingOccurrences(of: "https://", with: "wss://")
        
        guard let url = URL(string: wsURL) else {
            connectionState = .error("Invalid URL")
            return
        }
        
        // Create request with Cloudflare Access headers
        var request = URLRequest(url: url)
        request.addValue(config.clientId, forHTTPHeaderField: "CF-Access-Client-Id")
        request.addValue(config.clientSecret, forHTTPHeaderField: "CF-Access-Client-Secret")
        
        // Create WebSocket task
        let session = URLSession(configuration: .default, delegate: self, delegateQueue: nil)
        webSocketTask = session.webSocketTask(with: request)
        webSocketTask?.resume()
        
        connectionState = .connected
        
        // Start receiving messages
        receiveMessage()
    }
    
    func disconnect() {
        webSocketTask?.cancel(with: .goingAway, reason: nil)
        connectionState = .disconnected
    }
    
    // MARK: - ACP Protocol
    
    func sendInitialize() {
        let initializeRequest = """
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "capabilities": {},
                "clientInfo": {
                    "name": "iOS ACP Client",
                    "version": "1.0.0"
                }
            }
        }
        """
        
        send(message: initializeRequest)
    }
    
    func send(message: String) {
        let message = URLSessionWebSocketTask.Message.string(message)
        webSocketTask?.send(message) { error in
            if let error = error {
                print("WebSocket send error: \\(error)")
            }
        }
    }
    
    private func receiveMessage() {
        webSocketTask?.receive { [weak self] result in
            switch result {
            case .success(let message):
                switch message {
                case .string(let text):
                    print("Received: \\(text)")
                    self?.handleACPMessage(text)
                case .data(let data):
                    print("Received binary data: \\(data.count) bytes")
                @unknown default:
                    break
                }
                
                // Continue receiving
                self?.receiveMessage()
                
            case .failure(let error):
                print("WebSocket receive error: \\(error)")
                self?.connectionState = .error(error.localizedDescription)
            }
        }
    }
    
    private func handleACPMessage(_ message: String) {
        // Parse JSON-RPC 2.0 response
        // Handle initialize response, notifications, etc.
        // This is where you'd implement the full ACP protocol handling
    }
    
    // MARK: - Keychain Storage
    
    private func saveToKeychain() throws {
        guard let config = config else { return }
        
        let encoder = JSONEncoder()
        let data = try encoder.encode(config)
        
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrAccount as String: "acp-bridge-config",
            kSecValueData as String: data
        ]
        
        // Delete any existing item
        SecItemDelete(query as CFDictionary)
        
        // Add new item
        let status = SecItemAdd(query as CFDictionary, nil)
        guard status == errSecSuccess else {
            throw NSError(domain: "ACPBridge", code: Int(status), userInfo: [NSLocalizedDescriptionKey: "Failed to save to keychain"])
        }
    }
    
    func loadFromKeychain() throws {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrAccount as String: "acp-bridge-config",
            kSecReturnData as String: true
        ]
        
        var result: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        
        guard status == errSecSuccess, let data = result as? Data else {
            throw NSError(domain: "ACPBridge", code: Int(status), userInfo: [NSLocalizedDescriptionKey: "No saved configuration"])
        }
        
        let decoder = JSONDecoder()
        self.config = try decoder.decode(BridgeConnection.self, from: data)
    }
}

// MARK: - URLSessionWebSocketDelegate

extension ACPBridgeClient: URLSessionWebSocketDelegate {
    func urlSession(_ session: URLSession, webSocketTask: URLSessionWebSocketTask, didOpenWithProtocol protocol: String?) {
        print("WebSocket connected")
        connectionState = .connected
        
        // Send ACP initialize message
        sendInitialize()
    }
    
    func urlSession(_ session: URLSession, webSocketTask: URLSessionWebSocketTask, didCloseWith closeCode: URLSessionWebSocketTask.CloseCode, reason: Data?) {
        print("WebSocket closed: \\(closeCode)")
        connectionState = .disconnected
    }
}

// MARK: - SwiftUI View Example

struct BridgeSetupView: View {
    @StateObject private var client = ACPBridgeClient()
    @State private var showingQRScanner = false
    
    var body: some View {
        VStack(spacing: 20) {
            Text("ACP Bridge Connection")
                .font(.title)
            
            stateIndicator
            
            Button("Scan QR Code") {
                showingQRScanner = true
            }
            .buttonStyle(.borderedProminent)
            
            if client.connectionState == .connected {
                Button("Disconnect") {
                    client.disconnect()
                }
                .buttonStyle(.bordered)
            } else {
                Button("Connect") {
                    client.connect()
                }
                .buttonStyle(.bordered)
                .disabled(client.connectionState == .connecting)
            }
        }
        .padding()
        .sheet(isPresented: $showingQRScanner) {
            QRScannerView { qrString in
                do {
                    try client.configureFromQRCode(qrString)
                    showingQRScanner = false
                } catch {
                    print("Failed to configure: \\(error)")
                }
            }
        }
    }
    
    @ViewBuilder
    private var stateIndicator: some View {
        switch client.connectionState {
        case .disconnected:
            Label("Disconnected", systemImage: "circle.fill")
                .foregroundColor(.red)
        case .connecting:
            Label("Connecting...", systemImage: "circle.fill")
                .foregroundColor(.yellow)
        case .connected:
            Label("Connected", systemImage: "circle.fill")
                .foregroundColor(.green)
        case .error(let message):
            Label("Error: \\(message)", systemImage: "exclamationmark.triangle.fill")
                .foregroundColor(.red)
        }
    }
}

// Note: You'll need to implement QRScannerView using AVFoundation
// or a third-party library like CodeScanner
