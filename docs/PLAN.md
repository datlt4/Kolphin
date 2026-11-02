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

### M3 — Group / sort / hiển thị ✅ (v0.4.0, v0.5.0)
- ✅ Group by name / time / type / extension / size / **kích thước ảnh (W×H)** (đọc header ảnh ở nền); thu gọn từng nhóm; sort trong từng nhóm.
- ✅ Badge symlink (↪) / hardlink (×N).
- ✅ Thanh trượt cỡ icon + Ctrl+lăn chuột.
- ✅ Lưu chế độ xem / sort / group / nhóm đã thu gọn cho từng folder.
- ✅ Lọc nhanh (Ctrl+I, chữ hoặc `*.jpg`); chọn theo pattern (`*.jpg` hoặc `/regex/`); lấy mẫu ngẫu nhiên N file.
- ✅ Thống kê thư mục: số file theo đuôi, histogram dung lượng; dung lượng folder đệ quy (nền, huỷ được).
- ✅ Đổi icon cho file/folder bằng cách chọn trong bộ Material; menu chuột phải; hiện full tên khi chọn.

### M4 — Thao tác file (phần lớn ✅ v0.6.0)
- ✅ Cut/Copy/Paste cơ bản (clipboard chung với Nautilus).
- ✅ Tạo symlink / hardlink; emblem symlink kiểu Ubuntu.
- ✅ Hàng đợi copy/move chạy tuần tự, có % và tốc độ; trùng tên tự đổi "(copy)". Còn: pause/resume, hộp thoại xử lý trùng tên, kiểm tra lại bằng xxHash sau copy.
- ✅ Trash/xoá/đổi tên (F2), folder mới, **Ctrl+Z undo** (rename, move, copy, trash, link), đổi tên hàng loạt có xem trước (regex, `{n}`).
- ✅ Kéo thả vào/ra app khác (cùng ổ = move, khác ổ = copy; Shift/Ctrl để ép).
- ✅ "Mở bằng…" + đặt app mặc định.
- ✅ Properties + hash MD5 / SHA256 / SHA512 / xxHash3 trong **1 lần đọc**, tự so với hash trong clipboard.
- ✅ **Quick Look (Space)**: ảnh, khung hình video, text; ←/→ chuyển file. Còn: phát video (cần libgtk-4-media-gstreamer).
- **Mở file nén như folder** (zip/tar), menu nén/giải nén.

### M5 — Thiết bị & mạng ✅ (v0.7.0, trừ host Kermius)
- ✅ USB/ổ chưa mount: click để mount; ⏏ eject/unmount (udisks2); thanh dung lượng từng ổ trong sidebar.
- ✅ Connect to Server: SMB (chọn share khi chỉ nhập server) / SFTP / FTP / WebDAV qua gvfs (mật khẩu qua hộp thoại GTK + gnome-keyring); nhớ server đã dùng.
- **Host SFTP từ Kermius** hiện trong sidebar — chờ Kermius thêm `hosts.list` (đã ghi vào spec).

### M6 — Terminal, editor, AI ✅ (v0.8.0)
- ✅ Kermius Control API (F4): tab terminal riêng của Kolphin (icon Kolphin), tự `cd` theo folder (chỉ khi shell rảnh), đồng bộ ngược khi terminal `cd` (OSC 7), tự mở lại khi bị đóng.
- ✅ Không có Kermius → terminal mặc định + dòng gợi ý cài Kermius.
- ✅ **Custom Actions** (`~/.config/kolphin/actions.toml`, kiểu service menu Dolphin); có sẵn: Mở bằng VSCode, Claude, OpenCode (mở tab Kermius riêng), Share qua Kermius.
- ✅ **Badge git status** trên file/folder.
- ✅ **Command palette (Ctrl+K)** giống Kermius.
- ✅ Trash trong sidebar (cả Trash trên ổ NTFS): Restore, Empty Trash.

### M7 — Drop Shelf ✅ (v0.9.0)
- ✅ Giữ chuột trái + lắc ngang (≥4 lần đổi hướng trong 0,7 s) → kệ hiện cạnh con trỏ, luôn nổi trên cùng (X11, theo dõi chuột toàn màn hình qua XQueryPointer). Kệ giữ đường dẫn, kéo ra một lần là xong; Copy here / Move here vào thư mục đang mở; Ctrl+Shift+D mở kệ thủ công (cả Wayland).

### M8 — Tích hợp hệ thống (phần lớn ✅ v0.10.0)
- ✅ Đặt làm file manager mặc định (`xdg-mime` + D-Bus `org.freedesktop.FileManager1` cho "Show in folder"; hoàn tác được).
- Còn: backend portal FileChooser (app Snap/Flatpak/Chrome/Qt dùng portal; app GTK3 native không thay được).
- ✅ **Khôi phục phiên**: tabs + split view mở lại như lúc thoát.
- ✅ **Tìm kiếm song song** trong thư mục con (Ctrl+F), kết quả hiện dần.
- ✅ Tuỳ chọn tắt tự kiểm tra update. Còn: AppImage.

### Sau Linux
- Windows / macOS: lớp platform riêng (GIO/gvfs yếu ở đó), đóng gói `.msi` / `.app`.
