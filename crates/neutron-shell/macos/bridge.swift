// Native services for the Rust UI. One JSON request on stdin, one response on
// stdout. Secrets never appear in command-line arguments or diagnostic output.
import AppKit
import Foundation
import QuickLookThumbnailing
import Security

struct Request: Decodable {
    var action: String
    var paths: [String] = []
    var destination: String? = nil
    var name: String? = nil
    var move: Bool? = nil
    var permanent: Bool? = nil
    var target: String? = nil
    var secret: String? = nil
    var text: String? = nil
    // Optional rather than defaulted: synthesized decoding still requires a
    // key that has a default value, and most requests have no use for these.
    var outputs: [String]? = nil
    var size: Int? = nil
}
struct BridgeError: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}
func require(_ value: String?) throws -> String {
    guard let value else { throw BridgeError(message: "Missing request field") }
    return value
}
func checkStatus(_ status: OSStatus) throws {
    guard status == errSecSuccess else {
        let detail = SecCopyErrorMessageString(status, nil) as String? ?? "error \(status)"
        throw BridgeError(message: "Keychain operation failed: \(detail)")
    }
}
// Collision keys fold case and normalization, as the default APFS volume does.
func key(_ url: URL) -> String { url.path.precomposedStringWithCanonicalMapping.lowercased() }
func occupied(_ url: URL, _ seen: Set<String>) -> Bool {
    seen.contains(key(url)) || FileManager.default.fileExists(atPath: url.path)
        || (try? FileManager.default.destinationOfSymbolicLink(atPath: url.path)) != nil
}
// Finder's naming: "a.txt" -> "a copy.txt" -> "a copy 2.txt". Copying
// "a copy.txt" continues the sequence rather than making "a copy copy.txt".
func copyName(for source: URL, in folder: URL, _ seen: Set<String>) throws -> URL {
    var isDirectory: ObjCBool = false
    FileManager.default.fileExists(atPath: source.path, isDirectory: &isDirectory)
    let ext = isDirectory.boolValue ? "" : source.pathExtension
    var stem = ext.isEmpty ? source.lastPathComponent : source.deletingPathExtension().lastPathComponent
    if let range = stem.range(of: #" copy( [0-9]+)?$"#, options: .regularExpression) {
        stem.removeSubrange(range)
    }
    for n in 1...10000 {
        let name = (n == 1 ? "\(stem) copy" : "\(stem) copy \(n)") + (ext.isEmpty ? "" : ".\(ext)")
        let candidate = folder.appendingPathComponent(name)
        if !occupied(candidate, seen) { return candidate }
    }
    throw BridgeError(message: "Could not choose a name for a copy of \(source.lastPathComponent)")
}
// Runs every job even when some fail, then reports all the failures at once,
// so a batch is never left half done without saying which items were skipped.
func each(_ jobs: [(URL, URL)], verb: String, suffix: String = "", _ body: (URL, URL) throws -> Void) throws {
    var failures: [String] = []
    for (source, target) in jobs {
        do { try body(source, target) }
        catch let error as BridgeError { failures.append(error.message) }
        catch {
            let ns = error as NSError
            failures.append("\(source.lastPathComponent): \(ns.localizedFailureReason ?? ns.localizedDescription)")
        }
    }
    guard !failures.isEmpty else { return }
    if jobs.count == 1 { throw BridgeError(message: failures[0]) }
    throw BridgeError(message: "Could not \(verb) \(failures.count) of \(jobs.count) items\(suffix): " + failures.joined(separator: "; "))
}
/// Quick Look's own preview of `url` — the picture, the first page of a PDF, a
/// frame of a film — at most `side` pixels across, written to `out` as a PNG.
///
/// Only a real preview counts. Where Quick Look has nothing better than the
/// file's icon the answer is false, and the app keeps its own tile.
func thumbnail(_ url: URL, side: CGFloat, to out: URL) -> Bool {
    let request = QLThumbnailGenerator.Request(
        fileAt: url, size: CGSize(width: side, height: side), scale: 1, representationTypes: .thumbnail)
    final class Box: @unchecked Sendable { var image: CGImage? }
    let box = Box()
    let done = DispatchSemaphore(value: 0)
    QLThumbnailGenerator.shared.generateBestRepresentation(for: request) { representation, _ in
        box.image = representation?.cgImage
        done.signal()
    }
    // A preview that has not arrived by now is not arriving usefully: a disk
    // that is asleep, or a preview extension that has hung.
    if done.wait(timeout: .now() + 8) == .timedOut {
        QLThumbnailGenerator.shared.cancel(request)
        return false
    }
    guard let image = box.image,
          let png = NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:])
    else { return false }
    return (try? png.write(to: out, options: .atomic)) != nil
}

func run(_ r: Request) throws -> Any {
    let fm = FileManager.default
    let urls = r.paths.map { URL(fileURLWithPath: $0) }
    let cutType = NSPasteboard.PasteboardType("com.neutron.file-transfer")
    switch r.action {
    case "capacity":
        let path = try require(r.destination)
        let attributes = try fm.attributesOfFileSystem(forPath: path)
        // Mounts flagged "don't browse" (Recovery, VM, Preboot) stay out of the sidebar.
        var info = statfs()
        let browsable = statfs(path, &info) != 0 || info.f_flags & UInt32(MNT_DONTBROWSE) == 0
        // Ejectable as the Finder decides it: removable media, disk images
        // and network volumes, never the startup disk or an internal one.
        let keys: Set<URLResourceKey> = [.volumeIsEjectableKey, .volumeIsRemovableKey, .volumeIsLocalKey,
                                         .volumeIsInternalKey, .volumeIsRootFileSystemKey]
        let v = try? URL(fileURLWithPath: path).resourceValues(forKeys: keys)
        let root = v?.volumeIsRootFileSystem ?? (path == "/")
        let ejectable = !root && ((v?.volumeIsEjectable ?? false) || (v?.volumeIsRemovable ?? false)
            || !(v?.volumeIsLocal ?? true) || !(v?.volumeIsInternal ?? true))
        return ["total": (attributes[.systemSize] as? NSNumber)?.uint64Value ?? 0,
                "free": (attributes[.systemFreeSize] as? NSNumber)?.uint64Value ?? 0,
                "browsable": browsable, "ejectable": ejectable]
    case "copy", "move":
        let move = r.action == "move"
        let destination = URL(fileURLWithPath: try require(r.destination)).resolvingSymlinksInPath()
        // Validate the entire batch before changing anything. Never overwrite
        // an existing item, and never copy a directory into its own subtree.
        // A copy that collides gets Finder's "name copy" name; a move into the
        // folder it is already in is skipped.
        var seen = Set<String>()
        var jobs: [(URL, URL)] = []
        for source in urls {
            let canonical = source.resolvingSymlinksInPath().path
            if destination.path == canonical || destination.path.hasPrefix(canonical + "/") {
                throw BridgeError(message: "A folder cannot be transferred into itself")
            }
            if move && source.deletingLastPathComponent().resolvingSymlinksInPath().path == destination.path {
                continue
            }
            var target = destination.appendingPathComponent(source.lastPathComponent)
            if occupied(target, seen) {
                guard !move else {
                    throw BridgeError(message: "An item named \(target.lastPathComponent) already exists")
                }
                target = try copyName(for: source, in: destination, seen)
            }
            seen.insert(key(target))
            jobs.append((source, target))
        }
        try each(jobs, verb: move ? "move" : "copy") { source, target in
            if move { try fm.moveItem(at: source, to: target) }
            else { try fm.copyItem(at: source, to: target) }
        }
        return true
    case "delete":
        let permanent = r.permanent == true
        // Where each item went in the Trash, in order, so it can be put back.
        var trashed: [String] = []
        try each(urls.map { ($0, $0) }, verb: permanent ? "delete" : "move", suffix: permanent ? "" : " to the Trash") { url, _ in
            if permanent { try fm.removeItem(at: url); return }
            var resulting: NSURL?
            do { try fm.trashItem(at: url, resultingItemURL: &resulting) }
            catch let error as CocoaError where error.code == .featureUnsupported {
                throw BridgeError(message: "\(url.lastPathComponent) cannot be moved to the Trash on this volume")
            }
            trashed.append(resulting?.path ?? "")
        }
        return permanent ? true as Any : trashed as Any
    case "rename":
        let name = try require(r.name)
        guard !name.isEmpty, name != ".", name != "..", !name.contains("/"), !name.contains("\0"), let source = urls.first else {
            throw BridgeError(message: "Enter a single valid filename")
        }
        let target = source.deletingLastPathComponent().appendingPathComponent(name)
        if source.path != target.path { try fm.moveItem(at: source, to: target) }
        return true
    case "mkdir":
        let parent = URL(fileURLWithPath: try require(r.destination))
        for n in 1...10000 {
            let name = n == 1 ? "New Folder" : "New Folder (\(n))"
            do {
                try fm.createDirectory(at: parent.appendingPathComponent(name), withIntermediateDirectories: false)
                return name
            } catch let error as NSError where error.domain == NSCocoaErrorDomain && error.code == CocoaError.fileWriteFileExists.rawValue {
                continue
            }
        }
        throw BridgeError(message: "Could not choose a new folder name")
    case "clipboard-write":
        let pb = NSPasteboard.general
        pb.clearContents()
        guard pb.writeObjects(urls.map { $0 as NSURL }) else {
            throw BridgeError(message: "Could not write file URLs to the clipboard")
        }
        pb.setString(r.move == true ? "move" : "copy", forType: cutType)
        return true
    case "clipboard-text":
        let pb = NSPasteboard.general
        pb.clearContents()
        guard pb.setString(try require(r.text), forType: .string) else {
            throw BridgeError(message: "Could not write text to the clipboard")
        }
        return true
    case "reveal":
        // One Finder window with every item selected, where `open -R` would
        // reveal them one at a time.
        guard !urls.isEmpty else { throw BridgeError(message: "Nothing to show") }
        NSWorkspace.shared.activateFileViewerSelecting(urls)
        return true
    case "clipboard-read":
        let pb = NSPasteboard.general
        let items = pb.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL] ?? []
        return ["paths": items.map { $0.path }, "move": pb.string(forType: cutType) == "move"] as [String: Any]
    case "thumbnails":
        // Many to a request: starting this helper costs more than most
        // previews do to make.
        let outputs = r.outputs ?? []
        guard outputs.count == r.paths.count else { throw BridgeError(message: "Each preview needs somewhere to go") }
        let side = CGFloat(max(16, min(r.size ?? 256, 2048)))
        return zip(r.paths, outputs).map { path, out in
            thumbnail(URL(fileURLWithPath: path), side: side, to: URL(fileURLWithPath: out))
        }
    case "credential-store", "credential-load", "credential-clear":
        let query: [String: Any] = [kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: "Neutron", kSecAttrAccount as String: try require(r.target)]
        if r.action == "credential-load" {
            var q = query
            q[kSecReturnData as String] = true
            q[kSecMatchLimit as String] = kSecMatchLimitOne
            var result: CFTypeRef?
            let status = SecItemCopyMatching(q as CFDictionary, &result)
            if status == errSecItemNotFound { return NSNull() }
            try checkStatus(status)
            return (result as? Data).flatMap { String(data: $0, encoding: .utf8) } as Any? ?? NSNull()
        }
        if r.action == "credential-clear" {
            let status = SecItemDelete(query as CFDictionary)
            if status != errSecItemNotFound { try checkStatus(status) }
            return true
        }
        let data = Data(try require(r.secret).utf8)
        let values = [kSecValueData as String: data]
        let status = SecItemUpdate(query as CFDictionary, values as CFDictionary)
        if status == errSecItemNotFound {
            var item = query
            item[kSecValueData as String] = data
            try checkStatus(SecItemAdd(item as CFDictionary, nil))
        } else { try checkStatus(status) }
        return true
    default: throw BridgeError(message: "Unknown macOS operation")
    }
}
do {
    let request = try JSONDecoder().decode(Request.self, from: FileHandle.standardInput.readDataToEndOfFile())
    let result = try run(request)
    let output = try JSONSerialization.data(withJSONObject: ["result": result], options: [.fragmentsAllowed])
    FileHandle.standardOutput.write(output)
} catch {
    let output = try! JSONSerialization.data(withJSONObject: ["error": error.localizedDescription])
    FileHandle.standardOutput.write(output)
    exit(1)
}
