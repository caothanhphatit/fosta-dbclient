# DB Studio

Database client nhẹ (Tauri 2 + Rust, frontend TypeScript thuần) cho **MariaDB, MySQL, SQL Server và MongoDB**, với **workspace** và **startup tasks**: tự chạy `kubectl port-forward` khi mở workspace, tự reconnect khi rớt.

## Tính năng

- **Nhiều loại DB**: MariaDB và MySQL (`mysql_async`), SQL Server (`tiberius`, TLS bật mặc định), MongoDB (`mongodb`, hỗ trợ connection URI cho Atlas). Mỗi loại là một driver trong `src-tauri/src/` sau trait chung ở `drivers.rs`.
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
| Double-click connection | Kết nối |
| Double-click table | Mở dữ liệu |
| Ctrl+Enter / F9 | Chạy SQL (phần bôi đen, hoặc toàn bộ) |
| Enter / F2 / double-click ô | Sửa ô |
| Ctrl+S | Lưu thay đổi trên grid |
| F5 | Tải lại bảng |
