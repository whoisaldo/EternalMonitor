import XCTest
@testable import EternalMonitor

final class PairingCodeTests: XCTestCase {
    private func expectSuccess(
        _ value: String, host: String, port: UInt16,
        file: StaticString = #filePath, line: UInt = #line
    ) {
        switch PairingCode.parse(value) {
        case .success(let parsed):
            XCTAssertEqual(parsed.host, host, file: file, line: line)
            XCTAssertEqual(parsed.port, port, file: file, line: line)
        case .failure(let error):
            XCTFail("\(value) failed: \(error)", file: file, line: line)
        }
    }

    func testParsesIPv4() {
        expectSuccess("eternaldisplay://192.168.1.20:9876", host: "192.168.1.20", port: 9876)
    }

    func testParsesBracketedIPv6() {
        expectSuccess("eternaldisplay://[fe80::1]:9876", host: "fe80::1", port: 9876)
    }

    func testParsesHostname() {
        expectSuccess("eternaldisplay://ali-pc.local:9876", host: "ali-pc.local", port: 9876)
    }

    func testToleratesTrailingSlashAndWhitespace() {
        expectSuccess("  eternaldisplay://10.0.0.5:1234/  ", host: "10.0.0.5", port: 1234)
    }

    func testSchemeIsCaseInsensitive() {
        expectSuccess("EternalDisplay://10.0.0.5:9876", host: "10.0.0.5", port: 9876)
    }

    private func expectFailure(
        _ value: String, _ expected: PairingCode.ParseError,
        file: StaticString = #filePath, line: UInt = #line
    ) {
        switch PairingCode.parse(value) {
        case .success(let parsed):
            XCTFail("\(value) unexpectedly parsed as \(parsed)", file: file, line: line)
        case .failure(let error):
            XCTAssertEqual(error, expected, file: file, line: line)
        }
    }

    func testRejectsWrongScheme() {
        expectFailure("https://10.0.0.5:9876", .wrongScheme)
        expectFailure("not a url at all", .wrongScheme)
    }

    func testRejectsMissingPort() {
        expectFailure("eternaldisplay://10.0.0.5", .missingOrInvalidPort)
    }

    func testRejectsPortZero() {
        expectFailure("eternaldisplay://10.0.0.5:0", .missingOrInvalidPort)
    }
}
