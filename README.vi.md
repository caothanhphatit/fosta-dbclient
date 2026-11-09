# Fosta Studio

[English](README.md) · **Tiếng Việt**

Database client nhỏ gọn, chạy nhanh, hỗ trợ **11 engine**: MariaDB, MySQL, SQL Server, MongoDB, PostgreSQL, SQLite, Redis,
Elasticsearch, OpenSearch, ClickHouse và Oracle. Có bản desktop cho Windows và bản web chạy bằng Docker. Miễn phí, mã nguồn mở
(GPL-3.0).

**[Tải cho Windows](https://github.com/caothanhphatit/fosta-dbclient/releases/latest)** ·
[Website](https://dbcl.fosta.app/vi/) · [Changelog](CHANGELOG.md)

## Tính năng

- **11 engine, một giao diện**: cây database, lưới dữ liệu có kiểu và sửa trực tiếp, tab query, design view và sơ đồ ER, cùng các
  màn hình riêng cho từng engine (duyệt key Redis, Dev Tools và ES|QL cho Elasticsearch, mutation của ClickHouse, PL/SQL của Oracle).
- **Workspace** theo dự án hoặc môi trường, mỗi workspace có connection, tab và **startup task** riêng (ví dụ `kubectl port-forward`,
  tự chạy lại khi bị rớt).
- **SSH tunnel và TLS** cho mọi engine qua mạng: key, mật khẩu hoặc agent, jump host, kiểm tra host key, CA riêng, client
  certificate hoặc ghim chứng chỉ.
- **An toàn mặc định**: mọi câu lệnh được kiểm tra theo quy tắc của workspace (Open, Guarded, Read-only hoặc Custom). Workspace
  Production mặc định chỉ đọc, được chặn cả ở phía database, và lưu dữ liệu không bao giờ ghi đè dòng người khác vừa sửa.
- **Trình soạn query** gợi ý theo cột, format SQL, kết quả trả về dần với nút Stop dừng thật trên server, transaction và lịch sử
  theo từng tab.
- **Import và export**: nhập CSV, TSV, JSON; xuất CSV, CSV cho Excel, JSON, câu `INSERT` SQL và Markdown.
- **AI agent** (`Ctrl+Shift+A`) với provider và key của bạn (Fosta, OpenAI, Gemini, Claude, Ollama…). Agent hiểu schema, viết và
  sửa query, và chỉ đọc dữ liệu mẫu khi bạn cho phép.
- Giao diện sáng và tối, cấu hình gói trong một file JSON (portable), mật khẩu lưu trong Windows Credential Manager.

Chi tiết đầy đủ (tiếng Anh): [docs/features.md](docs/features.md).

## Bắt đầu

### Windows

Tải bộ cài hoặc file `.exe` portable ở [bản phát hành mới nhất](https://github.com/caothanhphatit/fosta-dbclient/releases/latest)
rồi chạy. File chưa được ký số nên Windows SmartScreen có thể hỏi xác nhận.

### Chạy bằng Docker (bản web)

```sh
docker run -d --name fosta-studio -p 127.0.0.1:8080:8080 -v fosta-studio-data:/data ghcr.io/caothanhphatit/fosta-studio
docker logs fosta-studio
```

Mở địa chỉ đăng nhập in trong log (`http://localhost:8080/?token=…`). Để kết nối database trên chính máy bạn, dùng
`host.docker.internal` thay cho `localhost`. Cấu hình, nâng cấp và HTTPS: [docs/web-edition.md](docs/web-edition.md).

### Oracle

Oracle cần Oracle Instant Client. Lần đầu kết nối, app sẽ đề nghị tải về giúp bạn ([chi tiết](docs/engines/oracle.md)).

### Build từ mã nguồn

Cần Node 20+ và Rust 1.88+:

```sh
npm install
npm run tauri dev
```

Build, test và database để test: [CONTRIBUTING.md](CONTRIBUTING.md).

## Phím tắt

| Phím            | Tác dụng                                |
| --------------- | --------------------------------------- |
| Ctrl+1 … 9      | Chuyển workspace                        |
| Ctrl+N / Ctrl+T | Connection mới / query mới              |
| Ctrl+Enter / F9 | Chạy SQL (phần đang chọn, hoặc toàn bộ) |
| Enter / F2      | Sửa ô đang chọn                         |
| Shift+Enter     | Mở ô trong trình soạn thảo              |
| Ctrl+S / F5     | Lưu thay đổi / tải lại bảng             |
| Ctrl+Shift+A    | AI agent                                |

## Thêm

- [ROADMAP.md](ROADMAP.md): những gì sắp làm.
- [ARCHITECTURE.md](ARCHITECTURE.md): kiến trúc bên trong.
- [docs/engines](docs/engines): ghi chú cho từng engine, SSH và TLS.

## Giấy phép

[GPL-3.0](LICENSE). Đóng góp được nhận theo [CLA](CLA.md), cho phép tác giả cung cấp thêm giấy phép thương mại. Fosta, provider
AI mặc định, là một dịch vụ riêng của cùng tác giả; không bắt buộc, mọi provider khác đều dùng được như nhau.
