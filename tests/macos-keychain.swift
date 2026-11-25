// From the repository root: swift -suppress-warnings tests/macos-keychain.swift
import Foundation
import Security

struct Failure: Error, CustomStringConvertible {
  let description: String
  init(_ s: String) { description = s }
}

func securityStatus(_ code: OSStatus) throws {
  if code != errSecSuccess { throw Failure("Security.framework returned \(code)") }
}

// Compare public item locators only. Never request kSecReturnData.
func accounts(in chain: SecKeychain? = nil) throws -> Set<String> {
  var query: [CFString: Any] = [
    kSecClass: kSecClassGenericPassword,
    kSecAttrService: "app.coda.cli.credentials.v1", kSecReturnAttributes: true,
    kSecMatchLimit: kSecMatchLimitAll,
  ]
  if let chain { query[kSecMatchSearchList] = [chain] }
  var value: CFTypeRef?
  let code = SecItemCopyMatching(query as CFDictionary, &value)
  if code == errSecItemNotFound { return [] }
  try securityStatus(code)
  guard let rows = value as? [[String: Any]] else { throw Failure("Invalid Keychain metadata") }
  return Set(rows.compactMap { $0[kSecAttrAccount as String] as? String })
}

func preference(_ operation: String) throws -> Data {
  let p = Process()
  let result = Pipe()
  p.executableURL = URL(fileURLWithPath: "/usr/bin/security")
  p.arguments = [operation, "-d", "user"]
  p.standardOutput = result
  try p.run()
  let data = result.fileHandleForReading.readDataToEndOfFile()
  p.waitUntilExit()
  guard p.terminationStatus == 0 else { throw Failure(operation) }
  return data
}

func create(_ url: URL, password: String) throws -> SecKeychain {
  var ref: SecKeychain?
  let code = password.withCString {
    SecKeychainCreate(url.path, UInt32(password.utf8.count), $0, false, nil, &ref)
  }
  try securityStatus(code)
  guard let ref else { throw Failure("Could not create temporary Keychain") }
  return ref
}

func cargo(_ args: [String], keychain: URL, stdin: Data? = nil, quiet: Bool = false) throws -> Int32
{
  let p = Process()
  p.executableURL = URL(fileURLWithPath: "/usr/bin/env")
  p.arguments = ["cargo"] + args
  var env = ProcessInfo.processInfo.environment
  env["CODA_KEYCHAIN_PATH"] = keychain.path
  p.environment = env
  if quiet {
    p.standardOutput = FileHandle.nullDevice
    p.standardError = FileHandle.nullDevice
  }
  let pipe = Pipe()
  if stdin != nil { p.standardInput = pipe }
  try p.run()
  if let stdin {
    pipe.fileHandleForWriting.write(stdin)
    pipe.fileHandleForWriting.closeFile()
  }
  p.waitUntilExit()
  return p.terminationStatus
}

struct UserKeychainState: Equatable {
  let defaultKeychain: Data
  let searchList: Data
  let itemLocators: Set<String>
  init() throws {
    defaultKeychain = try preference("default-keychain")
    searchList = try preference("list-keychains")
    itemLocators = try accounts()
  }
}

func runTests(in dir: URL, original: UserKeychainState) throws {
  let url = dir.appendingPathComponent("fixture.keychain-db")
  let password = UUID().uuidString
  let chain = try create(url, password: password)
  guard try UserKeychainState() == original else {
    throw Failure("User Keychain state changed when creating the private fixture")
  }
  let initArgs = [
    "run", "--quiet", "--locked", "--bin", "coda", "--", "init", "--provider", "openai", "--model",
    "test", "--base-url", "https://example.invalid/v1", "--api-key-stdin", "--data-dir",
  ]
  func initialize(_ at: URL, _ name: String) throws -> Int32 {
    try cargo(
      initArgs + [dir.appendingPathComponent(name).path], keychain: at,
      stdin: Data((UUID().uuidString + "\n").utf8), quiet: true)
  }
  guard try initialize(url, "positive") == 0, try accounts(in: chain).count == 1 else {
    throw Failure("CLI did not write exactly one fixture credential into its own test Keychain")
  }
  let missing = dir.appendingPathComponent("missing.keychain-db")
  guard try initialize(missing, "missing") != 0 else {
    throw Failure("CLI unexpectedly saved a credential with a missing Keychain")
  }
  guard
    try cargo(
      [
        "test", "--locked", "--lib", "missing_explicit_keychain_rejects_removal", "--", "--ignored",
      ], keychain: missing) == 0
  else {
    throw Failure("Missing Keychain deletion test failed")
  }
  try securityStatus(SecKeychainLock(chain))
  guard try initialize(url, "locked") != 0 else {
    throw Failure("CLI unexpectedly used a locked Keychain")
  }
  try password.withCString {
    try securityStatus(SecKeychainUnlock(chain, UInt32(password.utf8.count), $0, true))
  }
  let supplied = Array(CommandLine.arguments.dropFirst())
  let suites =
    supplied.isEmpty
    ? [
      ["test", "--locked", "--workspace"],
      ["test", "--locked", "--test", "credential_lifecycle", "--", "--ignored"],
    ] : [supplied]
  for args in suites {
    guard try cargo(args, keychain: url) == 0 else { throw Failure("Test command failed") }
  }
}

func run() throws {
  let original = try UserKeychainState()
  let dir = FileManager.default.temporaryDirectory.appendingPathComponent(
    "coda-tests-" + UUID().uuidString, isDirectory: true)
  try FileManager.default.createDirectory(
    at: dir, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
  var failures: [String] = []
  do { try runTests(in: dir, original: original) } catch {
    failures.append(String(describing: error))
  }
  // SecKeychainDelete can rewrite search-list preferences. This unlisted, file-based
  // Keychain is private to our directory. Its references are released by runTests.
  do { try FileManager.default.removeItem(at: dir) } catch {
    failures.append("Temporary directory cleanup failed: \(error)")
  }
  do {
    guard try UserKeychainState() == original else {
      throw Failure("Default Keychain, search list, or Coda item locators changed")
    }
    print("Checked user Keychain preferences and Coda item locators after cleanup.")
  } catch { failures.append(String(describing: error)) }
  guard failures.isEmpty else { throw Failure(failures.joined(separator: "\n")) }
}

do { try run() } catch {
  FileHandle.standardError.write(Data("\(error)\n".utf8))
  exit(1)
}
