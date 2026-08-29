import { cpSync, existsSync, mkdirSync, readdirSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const app = join(dirname(fileURLToPath(import.meta.url)), "..");
const icons = join(app, "src-tauri", "icons");
const source = join(icons, "icon-source.svg");
const catalog = join(app, "src-tauri", "Assets.xcassets");
const appIcon = join(catalog, "AppIcon.appiconset");
const generatedCatalogs = [
  join(icons, "ios"),
  join(app, "src-tauri", "gen", "apple", "Assets.xcassets", "AppIcon.appiconset"),
];
const info = { author: "xcode", version: 1 };

const images = [
  ["AppIcon-20x20@2x.png", "iphone", "2x", "20x20"],
  ["AppIcon-20x20@3x.png", "iphone", "3x", "20x20"],
  ["AppIcon-29x29@2x-1.png", "iphone", "2x", "29x29"],
  ["AppIcon-29x29@3x.png", "iphone", "3x", "29x29"],
  ["AppIcon-40x40@2x.png", "iphone", "2x", "40x40"],
  ["AppIcon-40x40@3x.png", "iphone", "3x", "40x40"],
  ["AppIcon-60x60@2x.png", "iphone", "2x", "60x60"],
  ["AppIcon-60x60@3x.png", "iphone", "3x", "60x60"],
  ["AppIcon-20x20@1x.png", "ipad", "1x", "20x20"],
  ["AppIcon-20x20@2x-1.png", "ipad", "2x", "20x20"],
  ["AppIcon-29x29@1x.png", "ipad", "1x", "29x29"],
  ["AppIcon-29x29@2x.png", "ipad", "2x", "29x29"],
  ["AppIcon-40x40@1x.png", "ipad", "1x", "40x40"],
  ["AppIcon-40x40@2x-1.png", "ipad", "2x", "40x40"],
  ["AppIcon-76x76@1x.png", "ipad", "1x", "76x76"],
  ["AppIcon-76x76@2x.png", "ipad", "2x", "76x76"],
  ["AppIcon-83.5x83.5@2x.png", "ipad", "2x", "83.5x83.5"],
  ["AppIcon-512@2x.png", "ios-marketing", "1x", "1024x1024"],
];

/**
 * Tauri owns the cross-platform rasterization; this only places its iOS
 * outputs into the catalog that Xcode consumes. ImageMagick re-encodes the
 * iOS PNGs as RGB because App Store validation rejects any alpha channel.
 */
execFileSync(
  process.platform === "win32" ? "npx.cmd" : "npx",
  ["--no-install", "tauri", "icon", source, "--output", icons, "--ios-color", "#08141e"],
  { cwd: app, stdio: "inherit" },
);

const generatedCatalog = generatedCatalogs.find(existsSync);
if (generatedCatalog === undefined) throw new Error("Tauri did not generate an iOS app icon catalog");

rmSync(catalog, { force: true, recursive: true });
mkdirSync(appIcon, { recursive: true });
cpSync(generatedCatalog, appIcon, { recursive: true });
if (generatedCatalog === generatedCatalogs[0]) rmSync(generatedCatalog, { force: true, recursive: true });

for (const filename of readdirSync(appIcon)) {
  if (!filename.endsWith(".png")) continue;

  const path = join(appIcon, filename);
  const rgbPath = `${path}.rgb.png`;
  execFileSync("magick", [path, "-background", "#08141e", "-alpha", "remove", "-alpha", "off", "-strip", rgbPath], {
    stdio: "inherit",
  });
  renameSync(rgbPath, path);
}

writeFileSync(join(catalog, "Contents.json"), `${JSON.stringify({ info }, null, 2)}\n`);
writeFileSync(
  join(appIcon, "Contents.json"),
  `${JSON.stringify({ images: images.map(([filename, idiom, scale, size]) => ({ filename, idiom, scale, size })), info }, null, 2)}\n`,
);
