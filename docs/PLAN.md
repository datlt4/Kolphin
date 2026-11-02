# Kolphin — Kế hoạch

File manager cho Linux viết bằng Rust, lấy cảm hứng từ Dolphin. Ưu tiên số 1: **mượt** khi mở thư mục cả triệu file trên HDD chậm.

## Quyết định đã chốt

- UI: GTK4 (gtk4-rs) + libadwaita, baseline GTK 4.6 / adw 1.1 (Ubuntu 22.04). Vùng hiển thị file là **canvas tự vẽ** (giống `KItemListView` của Dolphin): chỉ vẽ item trên màn hình.
- Linux trước (x86_64 + aarch64). Windows/macOS sau: lõi không phụ thuộc GUI, lớp platform tách riêng.
- Cài đặt hiển thị từng folder lưu tập trung ở `~/.local/share/kolphin/`, không ghi vào folder đang duyệt.
- Đóng gói `.deb` + `.tar.gz` (AppImage sau). Không Flatpak (sandbox cản terminal/USB/claude).
- Kermius: không sửa từ repo này; nhu cầu mới → cập nhật [kermius-control-api.md](kermius-control-api.md).
- Icon file/folder: [Material Icon Theme](https://github.com/material-extensions/vscode-material-icon-theme) (MIT), vendored ở `assets/material-icons/`, cập nhật bằng `tools/fetch-material-icons.sh`.

## Milestones

### M0 — Khung + phát hành ✅
- Cửa sổ GTK4, release CI (tag `v*` → `.deb` + `.tar.gz` amd64/arm64).
- Nút Install (lần đầu) / Update (bản mới) từ GitHub Releases, cài bằng `pkexec`, tự khởi động lại.

### M1 — Duyệt file cơ bản ✅
> Đo trên SSD, 1 triệu file: tên hiện sau 41 ms, list xong 0,7 s, stat xong 1,8 s, RSS 279 MB; vẽ mỗi frame < 1 ms. Renderer mặc định cairo (GL của GTK 4.6 rớt frame trên NVIDIA/X11).
> HDD (dataset thật): 46k file → tên sau 39 ms, stat xong 0,96 s; 26k ảnh → stat xong 2,2 s; cuộn ~75 fps.
- Canvas ảo hoá: 3 chế độ Icons / Compact (cuộn ngang như Dolphin) / Details; cuộn mượt (wheel có animation, touchpad kinetic).
- Đọc thư mục theo luồng nền: tên hiện ngay theo lô, `stat` (size/mtime) điền dần sau; sort tự nhiên (`img_2` < `img_10`), folder trước.
- Chọn: click, Ctrl/Shift+click, kéo khung, bàn phím (mũi tên, Home/End, PgUp/PgDn, Ctrl+A), gõ tên để nhảy tới file.
- Tabs (Ctrl+T/W), split view (F3), back/forward/up, ẩn/hiện file ẩn (Ctrl+H), F5 reload.
- Thanh đường dẫn: không focus → các nút theo cấp; focus (Ctrl+L / click) → ô nhập full path.
- Sidebar Places + các ổ đã mount.
- Status bar: số folder/file, dung lượng, phần đang chọn.
- Theo dõi thay đổi (inotify qua GIO), gom sự kiện rồi cập nhật tăng dần.
- Icon Material theo tên/đuôi file, render SVG bằng resvg, cache theo cỡ.
- Theme tối xanh navy + accent `#5b8cff` giống Kermius/Kellyfin; sidebar có logo + phiên bản.
- Benchmark: `tools/mkbench.py` tạo thư mục 1 triệu file; `KOLPHIN_BENCH=1` đo thời gian load + thời gian vẽ khi cuộn.

### M2 — Thumbnail ✅
- Ảnh + video; hàng đợi ưu tiên vùng đang xem, huỷ khi cuộn qua.
- Số luồng theo loại ổ (`/sys/block/*/queue/rotational`): HDD 1–2, SSD nhiều.
- JPEG giải mã thu nhỏ 1/8, ưu tiên thumbnail EXIF; video qua ffmpeg; cache chuẩn freedesktop `~/.cache/thumbnails`.

### M3 — Group / sort / hiển thị
- Group by name / time / type / extension / size / **kích thước ảnh (W×H)**; thu gọn từng nhóm; sort trong từng nhóm.
- Cột kích thước ảnh (chỉ đọc header).
- Thanh trượt cỡ icon + Ctrl+lăn chuột.
- Lưu chế độ xem / sort / group / nhóm đã thu gọn cho từng folder.
- Lọc nhanh (Ctrl+I), chọn theo glob/regex, lấy mẫu ngẫu nhiên N file.
- Thống kê thư mục: số file theo đuôi, histogram dung lượng; dung lượng folder đệ quy (nền, huỷ được).
- Đổi icon cho file/folder bằng cách chọn trong bộ Material.

### M4 — Thao tác file
- Hàng đợi copy/move: pause/resume, xử lý trùng tên, tốc độ; copy giữa các HDD chạy tuần tự; tuỳ chọn kiểm tra lại bằng xxHash sau copy.
- Trash/xoá/đổi tên, **Ctrl+Z undo**, đổi tên hàng loạt có xem trước (regex).
- Kéo thả vào/ra app khác.
- "Mở bằng…" + đặt app mặc định.
- Properties + hash MD5 / SHA256 / SHA512 / xxHash trong **1 lần đọc**, so với giá trị trong clipboard.
- **Quick Look (Space)**: xem nhanh ảnh/video/text, chế độ gallery.
- **Mở file nén như folder** (zip/tar), menu nén/giải nén.

### M5 — Thiết bị & mạng
- USB: hiện, unmount, safe-remove (udisks2); thanh dung lượng từng ổ trong sidebar.
- SMB + SFTP qua gvfs (mật khẩu lưu gnome-keyring).
- **Host SFTP từ Kermius** hiện trong sidebar.

### M6 — Terminal, editor, AI
- Kermius Control API: tab terminal riêng của Kolphin (icon Kolphin), tự `cd` theo folder (chỉ khi shell rảnh), đồng bộ ngược khi terminal `cd` (OSC 7), tự mở lại khi bị đóng.
- Không có Kermius → terminal mặc định + dòng gợi ý cài Kermius.
- **Custom Actions** (TOML, kiểu service menu Dolphin); có sẵn: Mở bằng VSCode, Claude, OpenCode (mở tab Kermius riêng), Share qua Kermius.
- **Badge git status** trên file/folder.
- **Command palette (Ctrl+K)** giống Kermius.

### M7 — Drop Shelf
- Lắc chuột khi đang kéo → hiện kệ thả tạm, thao tác rảnh tay rồi kéo ra sau. X11: theo dõi chuột toàn màn hình; Wayland: chỉ trong cửa sổ Kolphin + phím tắt.

### M8 — Tích hợp hệ thống
- Đặt làm file manager mặc định (`xdg-mime` + D-Bus `org.freedesktop.FileManager1` cho "Show in folder").
- Backend portal FileChooser (app Snap/Flatpak/Chrome/Qt dùng portal; app GTK3 native không thay được).
- **Khôi phục phiên**: tabs + split view mở lại như lúc thoát.
- **Tìm kiếm song song** kiểu `fd`, kết quả hiện dần.
- AppImage (tự thay file khi update); tuỳ chọn tắt tự kiểm tra update.

### Sau Linux
- Windows / macOS: lớp platform riêng (GIO/gvfs yếu ở đó), đóng gói `.msi` / `.app`.
