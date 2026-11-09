#!/bin/zsh
# dist/MornDeck.app(.zip) を作る。MornNotary が .app しか受け付けないため CLI を .app に包む。
set -euo pipefail
cd "${0:A:h}"
version=${VERSION:-$(cargo pkgid | sed 's/.*[#@]//')}
app=dist/MornDeck.app
rm -rf dist && mkdir -p $app/Contents/MacOS
for t in aarch64-apple-darwin x86_64-apple-darwin; do
    rustup target add $t >/dev/null
    cargo build --release --target $t
done
lipo -create target/{aarch64,x86_64}-apple-darwin/release/mdeck -output $app/Contents/MacOS/mdeck
cat > $app/Contents/Info.plist <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
    <key>CFBundleExecutable</key><string>mdeck</string>
    <key>CFBundleIdentifier</key><string>studio.tsukumi.MornDeck</string>
    <key>CFBundleName</key><string>MornDeck</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSUIElement</key><true/>
</dict></plist>
PLIST
codesign --force --sign "${SIGNING_IDENTITY:--}" --options runtime --identifier studio.tsukumi.MornDeck $app
ditto -c -k --sequesterRsrc --keepParent $app dist/MornDeck.app.zip
print "Built: $app ($version)"
