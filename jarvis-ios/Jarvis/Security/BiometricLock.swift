import Foundation
import LocalAuthentication

enum LocalUnlockResult: Equatable {
    case unlocked
    case cancelled
    case denied
    case unavailable(String)
}

struct BiometricLock {
    func unlock(reason: String) async -> LocalUnlockResult {
        let context = LAContext()
        context.localizedCancelTitle = "Stay Locked"
        var error: NSError?

        // Jarvis mobile permits the system device credential fallback through
        // Apple's owner-authentication policy; no credential enters this app.
        guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: &error) else {
            return .unavailable("Device authentication is not available.")
        }

        do {
            if try await context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: reason) {
                return .unlocked
            }
            return .denied
        } catch let error as LAError {
            switch error.code {
            case .appCancel, .systemCancel, .userCancel:
                return .cancelled
            case .biometryNotAvailable, .biometryNotEnrolled, .passcodeNotSet:
                return .unavailable("Set up Face ID, Touch ID, or a device passcode to unlock Jarvis.")
            default:
                return .denied
            }
        } catch {
            return .denied
        }
    }
}
