# DB Studio

[English](README.md) · **Tiếng Việt**

Database client nhẹ (Tauri 2 + Rust, frontend TypeScript thuần) cho **MariaDB, MySQL, SQL Server và MongoDB**, với **workspace** và **startup tasks**: tự chạy `kubectl port-forward` khi mở workspace, tự reconnect khi rớt.

## Tính năng

- **Nhiều loại DB**: MariaDB và MySQL (`mysql_async`), SQL Server (`tiberius`, TLS bật mặc định), MongoDB (`mongodb`, hỗ trợ connection URI cho Atlas). Mỗi loại là một driver trong `src-tauri/src/drivers/` sau trait chung `Driver` (`drivers/mod.rs`).
  - SQL Server: bảng hiển thị dạng `schema.table` (schema `dbo` được ẩn), phân trang bằng `OFFSET/FETCH`, DDL được dựng lại từ `INFORMATION_SCHEMA`.
  - MongoDB: collection hiển thị như bảng, cột là hợp của các field trong trang hiện tại, giá trị lồng nhau hiện dạng JSON. Ô filter nhận JSON query; tab Query chạy database command dạng JSON (`{ "find": "products", "filter": {...} }`).
- **Workspaces**: mỗi workspace (dự án, khách hàng, môi trường) có bộ connection, startup task và tab riêng. Chuyển bằng rail bên trái hoặc `Ctrl+1…9`; tab của workspace khác được giữ nguyên, không đóng.
- **Startup tasks**: chạy lệnh nền (vd. `kubectl port-forward`), tự restart có backoff, xem log realtime, process con bị kill khi app tắt. Task auto-start chạy khi workspace của nó được mở.
- **Connections**: lưu profile; mật khẩu lưu trong Windows Credential Manager, không ghi ra file.
- Cây Connection → Database → Table/View, tìm kiếm bảng.
- Mở bảng: phân trang, lọc bằng `WHERE` hoặc filter từng cột, sort theo cột, sửa trực tiếp trên ô, thêm/xoá dòng, Submit trong 1 transaction. Record panel hiện đầy đủ field và link theo foreign key.
- Query editor: syntax highlight, autocomplete tên bảng, chạy nhiều câu lệnh, Stop (KILL QUERY), mỗi tab giữ session riêng (BEGIN/COMMIT qua nhiều lần chạy được).
- Design view: danh sách cột + DDL.
- Vector (chỉ MariaDB 11.7+): tìm kiếm theo embedding, kiểm tra sức khoẻ cột/index, đo recall ANN so với exact.
- **Assistant** (panel bên phải, `Ctrl+Shift+A`): chat với model của bạn qua provider tuỳ chọn (Fosta mặc định với model `cx/gpt-6-astra`, OpenAI, Gemini, Claude, hoặc endpoint OpenAI-compatible tuỳ chỉnh như Ollama) bằng key của bạn, key lưu trong Credential Manager, request đi thẳng từ máy tới provider. Model chỉ thấy tên bảng/cột (tra bằng tool `describe_table`), nội dung editor, lỗi và log bạn hỏi; **không bao giờ chạy query**. Mỗi code block có nút Insert / New tab để bạn tự chạy trong editor. Quick action theo ngữ cảnh: giải thích, tối ưu, sửa lỗi, dịch giữa MySQL ↔ SQL Server ↔ Mongo, viết aggregation pipeline, sinh dữ liệu test, giải thích log startup task.
- Theme sáng / tối / theo hệ thống (nút dưới rail).

Config (workspaces, connections, tasks) lưu ở `db-studio.json` cạnh file `.exe`, nên app là portable. File config cũ (chỉ có `connections` + `tasks`) được tự chuyển thành một workspace khi mở.

## Cài toolchain

Cần Node 20+, Rust stable và một trong hai:

- **MSVC** (khuyến nghị cho bản release): VS Build Tools với C++ x64 + Windows SDK. Script dưới cài vào `D:\tools` (cần admin một lần):

  ```powershell
  powershell -ExecutionPolicy Bypass -File scripts\setup-toolchain.ps1
  ```

- **GNU**: toolchain `x86_64-pc-windows-gnu` cần `dlltool.exe` của MinGW trên PATH (vd. `C:\msys64\mingw64\bin`).

## Chạy

```powershell
npm install
npm run tauri dev
```

Xem nhanh giao diện trong trình duyệt, không cần MariaDB (backend được giả lập bằng dữ liệu mẫu, chỉ ở chế độ dev):

```powershell
npm run dev
```

Database test bằng Docker (MariaDB 11.8 `shop` có cột vector, SQL Server `ERP`, MongoDB `events`, đều có dữ liệu giả; tài khoản ghi trong `dev/docker-compose.yml`):

```powershell
docker compose -f dev/docker-compose.yml up -d
sh dev/seed-mssql.sh
cd src-tauri; cargo test live_ -- --ignored --test-threads=1
```

Build file `.exe` (không cần installer):

```powershell
npm run tauri build -- --no-bundle
```

File chạy: `src-tauri\target\release\db-studio.exe`.

## Phím tắt

| Phím | Tác dụng |
|---|---|
| Ctrl+1 … 9 | Chuyển workspace |
| Ctrl+N | Connection mới |
| Ctrl+T | Query mới |
| Ctrl+Shift+A | Bật/tắt panel Assistant |
| Double-click connection | Kết nối |
| Double-click table | Mở dữ liệu |
| Ctrl+Enter / F9 | Chạy SQL (phần bôi đen, hoặc toàn bộ) |
| Enter / F2 / double-click ô | Sửa ô |
| Ctrl+S | Lưu thay đổi trên grid |
| F5 | Tải lại bảng |

## Đóng góp

Xem [CONTRIBUTING.md](CONTRIBUTING.md) (cài đặt, chạy test, quy ước code) và [ARCHITECTURE.md](ARCHITECTURE.md) (cấu trúc, luồng dữ liệu, cách thêm driver).

## Giấy phép

DB Studio là phần mềm tự do theo [GNU General Public License v3.0](LICENSE). Đóng góp được nhận theo
[Contributor License Agreement](CLA.md), cho phép người duy trì cấp thêm license thương mại (ví dụ cho công ty muốn nhúng DB Studio
mà không theo các nghĩa vụ của GPL) trong khi dự án vẫn giữ GPL.

### Về Fosta

Fosta, provider mặc định của Assistant, là dịch vụ API AI riêng do tác giả DB Studio vận hành. Nó được xếp đầu chỉ cho tiện:
mọi provider khác (OpenAI, Gemini, Claude, hay endpoint tương thích OpenAI như Ollama chạy trên máy) đều dùng được như nhau,
DB Studio không bắt buộc tài khoản Fosta, và request chỉ đi tới provider bạn cấu hình.
