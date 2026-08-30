import XCTest
@testable import Jarvis

final class ClientDTOTests: XCTestCase {
    func testEnrollmentUsesBackendFieldNames() throws {
        let encoded = try JSONEncoder().encode(
            EnrollmentRequest(name: "Gus's iPhone", platform: "ios", publicKey: "ab")
        )
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: encoded) as? [String: String])
        XCTAssertEqual(json["public_key"], "ab")
        XCTAssertNil(json["publicKey"])
        XCTAssertEqual(json["platform"], "ios")
    }

    func testDecodesCurrentPairingContract() throws {
        let data = #"{"request_id":"018f47de-936a-7000-8000-000000000001","nonce":"00","expires_at":1800000000}"#.data(using: .utf8)!
        let decoded = try JSONDecoder().decode(PairingRequestResponse.self, from: data)
        XCTAssertEqual(decoded.expiresAt, 1_800_000_000)
        XCTAssertEqual(decoded.nonce, "00")
    }

    func testDecodesCurrentConversationContract() throws {
        let data = #"{"id":"018f47de-936a-7000-8000-000000000001","title":"Hello","messages":[{"role":"assistant","content":"Hi","model":null,"at":"2026-08-30T10:00:00Z"}]}"#.data(using: .utf8)!
        let decoded = try JSONDecoder().decode(ConversationResponse.self, from: data)
        XCTAssertEqual(decoded.messages.first?.content, "Hi")
        XCTAssertTrue(decoded.messages.first?.isAssistant == true)
    }

    func testHexChallengeRejectsMalformedInput() {
        XCTAssertNil(Data(hexEncoded: "0"))
        XCTAssertNil(Data(hexEncoded: "zz"))
        XCTAssertEqual(Data(hexEncoded: "00ff"), Data([0, 255]))
    }
}
