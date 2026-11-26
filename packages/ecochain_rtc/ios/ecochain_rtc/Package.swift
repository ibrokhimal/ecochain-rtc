// swift-tools-version: 5.9
// The swift-tools-version declares the minimum version of Swift required to build this package.

import PackageDescription

let package = Package(
    name: "ecochain_rtc",
    platforms: [
        .iOS("13.0")
    ],
    products: [
        .library(name: "ecochain-rtc", targets: ["ecochain_rtc"])
    ],
    dependencies: [
        .package(name: "FlutterFramework", path: "../FlutterFramework"),
        .package(name: "flutter_webrtc", path: "../flutter_webrtc")
    ],
    targets: [
        .target(
            name: "ecochain_rtc",
            dependencies: [
                .product(name: "FlutterFramework", package: "FlutterFramework"),
                .product(name: "WebRTC", package: "flutter_webrtc")
            ]
        )
    ]
)
