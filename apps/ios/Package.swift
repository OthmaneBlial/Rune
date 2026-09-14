// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "RuneIOS",
    platforms: [.iOS(.v17)],
    products: [
        .executable(name: "RuneIOS", targets: ["RuneIOS"]),
    ],
    targets: [
        .target(
            name: "RuneFFIHeaders",
            path: "Sources/RuneFFIHeaders",
            publicHeadersPath: "include"
        ),
        .executableTarget(
            name: "RuneIOS",
            dependencies: ["RuneFFIHeaders"]
        ),
    ]
)
