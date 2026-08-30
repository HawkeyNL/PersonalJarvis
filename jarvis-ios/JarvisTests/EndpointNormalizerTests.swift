import XCTest
@testable import Jarvis

final class EndpointNormalizerTests: XCTestCase {
    func testAddsHTTPSAndRemovesTrailingSlash() throws {
        XCTAssertEqual(
            try EndpointNormalizer.normalize(" jarvis.local/ ").absoluteString,
            "https://jarvis.local"
        )
    }

    func testPreservesExplicitLANHTTPPort() throws {
        XCTAssertEqual(
            try EndpointNormalizer.normalize("http://192.168.1.24:8080").absoluteString,
            "http://192.168.1.24:8080"
        )
    }

    func testRejectsCredentialsAndNonHTTPProtocols() {
        XCTAssertThrowsError(try EndpointNormalizer.normalize("https://owner:secret@jarvis.local"))
        XCTAssertThrowsError(try EndpointNormalizer.normalize("file:///tmp/jarvis"))
    }

    func testRejectsAPIPathToPreventDoubleVersionedRoutes() {
        XCTAssertThrowsError(try EndpointNormalizer.normalize("https://jarvis.local/v1"))
    }

    func testRejectsPublicCleartextEndpoint() {
        XCTAssertThrowsError(try EndpointNormalizer.normalize("http://example.com")) { error in
            XCTAssertEqual(error as? EndpointValidationError, .insecureRemote)
        }
    }
}
