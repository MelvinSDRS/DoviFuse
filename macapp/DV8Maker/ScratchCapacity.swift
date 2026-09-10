import Foundation

enum ScratchCapacity {
    static func availableBytes(at url: URL) throws -> Int64 {
        let values = try url.resourceValues(forKeys: [.volumeIsLocalKey])
        // SMB can return zero for important-usage capacity despite ample free space.
        // Only local volumes support using that estimate of reclaimable capacity.
        if values.volumeIsLocal == true,
           let important = try? url.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey]),
           let available = important.volumeAvailableCapacityForImportantUsage,
           available > 0 {
            return available
        }
        let attributes = try FileManager.default.attributesOfFileSystem(forPath: url.path)
        guard let available = attributes[.systemFreeSize] as? NSNumber else {
            throw CocoaError(.fileReadUnknown)
        }
        return available.int64Value
    }
}
