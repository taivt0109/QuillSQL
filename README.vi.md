<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logo/png/quillsql-logo-dark.png">
    <img alt="QuillSQL" src="assets/logo/png/quillsql-logo.png" width="560">
  </picture>
</p>

<p align="center">
  <a href="README.md">English</a> · <b>Tiếng Việt</b>
</p>

<p align="center">
  <a href="https://github.com/taivt0109/QuillSQL/releases/latest"><img alt="Bản phát hành mới nhất" src="https://img.shields.io/github/v/release/taivt0109/QuillSQL"></a>
  <a href="https://github.com/taivt0109/QuillSQL/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/taivt0109/QuillSQL/actions/workflows/ci.yml/badge.svg"></a>
  <a href="LICENSE"><img alt="Giấy phép MIT" src="https://img.shields.io/badge/license-MIT-blue"></a>
</p>

**QuillSQL** giúp viết T-SQL trong SQL Server Management Studio nhanh hơn và an toàn hơn. Nó:
- format code;
- gợi ý bảng, cột và điều kiện JOIN ngay khi gõ;
- mở rộng snippet, refactor, cảnh báo code có rủi ro;
- lưu lịch sử query;
- nhờ Claude viết, giải thích hoặc tối ưu SQL dựa trên schema và execution plan thật của bạn.

QuillSQL miễn phí, mã nguồn mở. Engine viết bằng Rust, chạy nhanh, và dùng được cả trên dòng lệnh lẫn trong CI.

## QuillSQL làm được gì

| | |
|---|---|
| **Format** | Một phím tắt sắp xếp lại cả script hoặc đoạn đang chọn: danh sách cột, JOIN, `CASE`, `AND`/`OR`, chữ hoa/thường, dấu chấm phẩy. Tùy chỉnh được. Phần nào không kiểm chứng được là đúng thì không thay đổi. |
| **IntelliSense** | Gợi ý bảng, view, cột, procedure, schema, CTE và biến ngay khi gõ, lấy từ database đang kết nối. |
| **Viết code giúp bạn** | Sinh điều kiện JOIN từ khóa ngoại, `EXEC` kèm đủ tham số, `INSERT ... VALUES` và `UPDATE ... SET` kèm đủ cột, mở `*` thành danh sách cột. |
| **Snippet** | Khoảng 70 snippet có sẵn (`ssf` + Tab → `SELECT * FROM`), thêm snippet riêng bằng JSON, bọc đoạn đang chọn bằng snippet. |
| **Refactor** | Thêm schema/alias cho tên, thêm hoặc bỏ `AS`, đổi tên thông minh, gói thành stored procedure, inline một lệnh `EXEC`. Mỗi thao tác hoàn tác được bằng một lần Undo. |
| **Phân tích code** | 54 rule gạch chân lỗi ngay khi gõ (`SELECT *`, `= NULL`, `UPDATE` không có `WHERE`, cú pháp lỗi thời…). Sửa tất cả bằng một lệnh. |
| **AI với Claude** | Viết SQL từ một câu mô tả, giải thích hoặc review script, phân tích execution plan và Query Store dựa trên index và số dòng thật. |
| **Nhận biết nhanh** | Tô màu tab và editor của kết nối production; rê chuột để xem cột của bảng; tô sáng mọi chỗ dùng cùng một tên. |
| **Công cụ database** | Tìm procedure và view đang tham chiếu tới object không còn tồn tại; tìm lại mọi query bạn đã chạy. |
| **Dòng lệnh** | `quill` format cả thư mục file `.sql` và chạy phân tích code trong CI. |

## Cài đặt

Yêu cầu:
- Windows có SQL Server Management Studio 21 trở lên.
- Các lệnh AI cần thêm [Claude Code CLI](https://code.claude.com/docs/en/overview), đã cài và đăng nhập.

Đóng SSMS, rồi chạy trong PowerShell:

```powershell
irm https://raw.githubusercontent.com/taivt0109/QuillSQL/main/install.ps1 | iex
```

Lệnh này:
- cài extension vào mọi SSMS 21 trở lên trên máy;
- cài công cụ dòng lệnh `quill` vào `%LOCALAPPDATA%\QuillSQL\bin` và thêm thư mục đó vào PATH;
- không cần quyền admin;
- kiểm tra mọi file tải về với `SHA256SUMS.txt` của bản phát hành trước khi cài.

Mở SSMS lên: mọi thứ nằm trong menu **Tools > QuillSQL**.

Chạy lại đúng lệnh trên để cập nhật. Muốn chọn tùy chọn thì đặt biến môi trường trước:

```powershell
$env:QUILLSQL_VERSION = '0.2.0'    # cài một phiên bản cụ thể
$env:QUILLSQL_NO_CLI = '1'         # chỉ cài extension cho SSMS
$env:QUILLSQL_UNINSTALL = '1'      # gỡ QuillSQL
irm https://raw.githubusercontent.com/taivt0109/QuillSQL/main/install.ps1 | iex
```

### Cập nhật

Mỗi ngày một lần, QuillSQL kiểm tra GitHub xem có bản mới không. Nếu có, nó tải về, kiểm tra checksum, rồi hiện một thanh thông báo ở đầu SSMS với ba lựa chọn:
- **Install when SSMS closes**: cài khi đóng SSMS;
- **What's new**: xem có gì mới;
- **Skip this version**: bỏ qua phiên bản này.

SSMS không cho thay extension khi đang chạy, nên bản cập nhật được cài ngay sau khi SSMS đóng.
- Kiểm tra ngay: **Tools > QuillSQL > Check for Updates**.
- Tắt kiểm tra hằng ngày: tạo file `%LOCALAPPDATA%\QuillSQL\settings.json` với nội dung `{ "autoUpdate": false }`.

## Chi tiết tính năng

### Format

**Tools > QuillSQL > Format T-SQL** (Ctrl+Shift+Alt+F) format đoạn đang chọn, hoặc cả script nếu không chọn gì. Một lần Undo là trả lại như cũ.

```sql
-- Trước
select c.Name,sum(o.Total) total from dbo.Customers c join sales.Orders o on o.CustomerId=c.CustomerId where o.Total>100 and c.Name like 'A%' group by c.Name

-- Sau, với "applyCasing": true
SELECT c.Name,
       sum(o.Total) total
FROM dbo.Customers c
JOIN sales.Orders o
    ON o.CustomerId = c.CustomerId
WHERE o.Total > 100
    AND c.Name LIKE 'A%'
GROUP BY c.Name
```

Bộ format làm việc trên token. Sau khi format, nó đọc lại kết quả và so với bản gốc. Nó báo lỗi và giữ nguyên code của bạn nếu:
- có gì đó thay đổi ngoài khoảng trắng và những phần bạn yêu cầu viết lại; hoặc
- script có chuỗi, comment hay tên trong ngoặc chưa đóng.

Các dòng nằm giữa `-- quill-format-off` và `-- quill-format-on` không bao giờ bị đụng tới. Xem [Tùy chọn format](#tùy-chọn-format) để đổi cách trình bày.

### IntelliSense

Khi gõ, danh sách gợi ý hiện ra:

- bảng, view, hàm trả về bảng, schema và CTE sau `FROM`, `JOIN`, `INTO` và `UPDATE`;
- procedure sau `EXEC`; object sau `schema.`;
- cột sau `alias.` hoặc `table.`;
- cột của mọi bảng trong câu lệnh, khi gõ trong `SELECT`, `WHERE` và `ON`. Tên cột có ở hai bảng sẽ được chèn kèm alias;
- cột trong `INSERT INTO t (`; biến đã khai báo trong script sau `@`.

Phím tắt: Up/Down để di chuyển, Enter hoặc Tab để chèn, Esc để đóng, Ctrl+Space để mở danh sách.

Metadata được nạp ngầm ở lần đầu dùng một database và tự đổi theo `USE`. Sau khi sửa schema, chạy **Tools > QuillSQL > Refresh T-SQL Suggestions** để nạp lại.

Danh sách còn viết code giúp bạn:

- **Sau `JOIN`:** gợi ý các bảng có khóa ngoại liên quan, kèm alias và điều kiện, ví dụ `sales.Orders o ON o.CustomerId = c.CustomerId`. Sau `ON` thì chỉ gợi ý điều kiện.
- **Sau `EXEC`:** procedure kèm đủ tham số, mỗi tham số một dòng có ghi kiểu dữ liệu.
- **Sau `INSERT INTO table `:** danh sách cột và một dòng `VALUES` có sẵn chỗ điền cho từng cột.
- **Sau `UPDATE table SET `:** tất cả các cột. Cột identity, computed và rowversion được bỏ qua.

QuillSQL tắt "Auto list members" của SSMS để không hiện hai danh sách cùng lúc. Muốn bật lại thì vào Tools > Options > Text Editor > Transact-SQL > IntelliSense.

### Snippet

Gõ prefix rồi nhấn Tab, hoặc chọn từ danh sách gợi ý. **Tools > QuillSQL > Insert Snippet...** liệt kê tất cả snippet. Khi đang chọn một đoạn code, snippet sẽ bọc quanh đoạn đó, ví dụ:
- `tc`: TRY...CATCH;
- `be`: BEGIN...END;
- `timings`: đo thời gian chạy.

Snippet riêng là các file JSON đặt trong `%LOCALAPPDATA%\QuillSQL\Snippets`. Snippet trùng prefix với snippet có sẵn sẽ thay thế snippet đó. Thêm xong thì khởi động lại SSMS.

```json
{ "prefix": "sel", "description": "Select from a table", "body": "SELECT $CURSOR$ FROM $table$;",
  "placeholders": [{ "name": "table", "defaultValue": "dbo.MyTable" }] }
```

Phần thân snippet dùng được các biến sau:

| Biến | Ý nghĩa |
|---|---|
| `$CURSOR$` | vị trí con trỏ sau khi chèn |
| `$SELECTEDTEXT$` | đoạn đang chọn |
| `$SELECTIONSTART$`, `$SELECTIONEND$` | vùng sẽ được chọn sau khi chèn |
| `$DATE$`, `$TIME$` | ngày, giờ hiện tại |
| `$USER$`, `$MACHINE$` | tên người dùng, tên máy |
| `$SERVER$`, `$DBNAME$` | server, database đang kết nối |
| `$PASTE$` | nội dung clipboard |

Placeholder `$tên$` đầu tiên được chọn sẵn sau khi chèn để bạn gõ đè lên.

### Refactor

Trong **Tools > QuillSQL**:

- **Expand Wildcards:** đổi `*` và `alias.*` thành danh sách cột. Nhấn Tab ngay sau một dấu `*` cũng làm việc này cho riêng dấu `*` đó.
- **Qualify Object Names:** thêm schema cho bảng, view và procedure; thêm alias cho cột trong các query dùng nhiều bảng.
- **Add AS to Aliases** / **Remove AS from Aliases.**
- **Smart Rename...:** đổi tên biến (trong batch của nó), alias hoặc CTE (trong câu lệnh của nó) tại vị trí con trỏ. Với bảng, view, procedure hoặc cột, lệnh này mở một script `sp_rename` để bạn xem lại rồi tự chạy. Script kèm một query liệt kê các object đang dùng tên cũ.
- **Encapsulate as Stored Procedure...:** biến đoạn đang chọn thành script `CREATE PROCEDURE`. Các biến được dùng mà chưa khai báo trở thành tham số có kiểu.
- **Inline EXEC:** thay lệnh `EXEC` tại con trỏ bằng thân procedure. Mỗi tham số được khai báo với giá trị truyền vào, hoặc giá trị mặc định nếu không truyền.

### Phân tích code

Lỗi được gạch chân sau khi bạn ngừng gõ một chút; rê chuột lên để đọc.
- **Analyze Code:** liệt kê lỗi trong Error List.
- **Fix Code Issues:** áp dụng một lượt mọi cách sửa an toàn:
  - `!=` thành `<>`;
  - `= NULL` thành `IS NULL`;
  - `@@IDENTITY` thành `SCOPE_IDENTITY()`;
  - `TOP 5` thành `TOP (5)`;
  - thêm `WITH` cho table hint;
  - thêm schema còn thiếu;
  - mở `SELECT *` và thêm danh sách cột cho `INSERT`.
- **Toggle Code Analysis Underlines:** bật/tắt gạch chân.

Mỗi rule có một mã cố định; tiền tố cho biết loại rule:

| Tiền tố | Loại |
|---|---|
| BP | best practice |
| DEP | cú pháp lỗi thời |
| EI | lỗi khi thực thi |
| MI | khác |
| PE | hiệu năng |
| ST | phong cách |
| SC | script |

`RULES` trong [`src/analysis.rs`](src/analysis.rs) liệt kê đủ 54 rule.

### AI với Claude

Các lệnh này dùng Claude Code CLI trên máy bạn. Mọi tool của Claude đều bị tắt, nên Claude chỉ có thể trả lời, không làm gì khác trên máy.

- **Ask AI...:** viết SQL từ một yêu cầu ("10 khách hàng có tổng đơn hàng cao nhất năm nay"), hoặc viết lại đoạn đang chọn ("thêm phân trang"). Kết quả được chèn vào và chọn sẵn; một lần Undo là bỏ.
- **Explain with AI:** giải thích đoạn đang chọn hoặc cả script, hiện trong cửa sổ Output.
- **Review with AI:** liệt kê vấn đề và đưa ra script đã cải thiện, trong một cửa sổ mới.
- **Analyze Plan with AI:** đọc số dòng, dung lượng, index và ngày cập nhật statistics của các bảng mà query dùng, cùng estimated execution plan (chỉ biên dịch, không chạy query). Sau đó hỏi Claude chi phí nằm ở đâu và nên sửa gì.
- **Analyze Actual Plan with AI (runs the query)...:** như trên nhưng dùng số liệu đo thật (số dòng, số lần đọc, thời gian của từng bước). Lệnh này chạy query rồi bỏ kết quả, nên chỉ nhận script chỉ đọc và hỏi bạn trước khi chạy.
- **Find Slow Queries with AI (Query Store):** lấy 10 query tốn nhất trong 7 ngày qua, plan của các query chậm nhất và index của các bảng liên quan, rồi nhờ Claude đề xuất những thay đổi có tác động lớn nhất.

Các lệnh phân tích plan hiện toàn bộ dữ liệu gửi cho Claude trong cửa sổ Output.

Cài đặt tùy chọn nằm trong `%LOCALAPPDATA%\QuillSQL\ai.json`, ví dụ `{ "model": "sonnet", "language": "Vietnamese" }`. `language` là ngôn ngữ của phần giải thích và comment.

### Nhận biết nhanh

- **Tô màu tab:** cửa sổ query kết nối tới server bạn đánh dấu sẽ có tab và thanh trên đầu editor được tô màu. Tạo file `%USERPROFILE%\.quillsql-colors.json`:

  ```json
  [{ "match": "prod", "color": "#C62828", "label": "PRODUCTION" },
   { "match": "test", "color": "#2E7D32" }]
  ```

  `match` được tìm trong chuỗi "server / database", không phân biệt hoa thường; quy tắc khớp đầu tiên được dùng.
- **Rê chuột:** rê lên bảng, cột, alias hoặc biến (hoặc nhấn Ctrl+K, Ctrl+I) để xem cột, kiểu dữ liệu hoặc tham số.
- **Tô sáng:** mọi chỗ dùng biến, alias hoặc tên tại con trỏ được tô sáng.

### Công cụ database

- **Find Invalid Objects:** liệt kê procedure, view và function đang tham chiếu tới object không còn tồn tại. Kết quả hiện trong cửa sổ Output.
- **Query History...:** tìm trong mọi query bạn đã chạy (F5) và mọi cửa sổ query đã đóng, kèm server và database. Có thể mở lại trong cửa sổ mới hoặc sao chép.

## Quyền riêng tư

QuillSQL chạy trên máy bạn và chỉ kết nối tới:

- **SQL Server của bạn:** dùng chính kết nối của cửa sổ query, để đọc metadata, plan và Query Store.
- **GitHub:** mỗi ngày một lần, để kiểm tra cập nhật. Tắt bằng `"autoUpdate": false`.
- **Claude:** qua Claude Code CLI của bạn, và chỉ khi bạn chạy một lệnh AI. Dữ liệu gửi đi gồm:
  - yêu cầu của bạn, đoạn đang chọn hoặc cả script, và tên/kiểu của các bảng mà script dùng;
  - với lệnh phân tích plan: thêm số dòng, định nghĩa index và bản tóm tắt plan (có chứa các giá trị literal và tham số trong query);
  - với lệnh Query Store: thêm nội dung các query chậm nhất.

  Dữ liệu dòng của bảng không bao giờ được gửi đi.

Lịch sử query được lưu ở `%LOCALAPPDATA%\QuillSQL\history.jsonl` (5000 mục gần nhất). File này chứa toàn văn script của bạn; nếu script có thông tin bí mật thì hãy xóa file.

## Dòng lệnh

```powershell
quill -p .\scripts                # format mọi file .sql, kể cả thư mục con
quill -p .\scripts --check        # chỉ báo cáo; mã thoát 1 nếu có file sẽ bị thay đổi
quill -p a.sql -b                 # giữ bản gốc thành a.sql.bak
Get-Content a.sql | quill --stdin # đọc từ stdin, ghi ra stdout
quill --analyze -p .\scripts      # phân tích code, dạng file:dòng:cột: mức-độ MÃ thông-báo
```

Mã thoát:
- `0`: thành công;
- `1`: có thay đổi (`--check`) hoặc có cảnh báo (`--analyze`);
- `2`: lỗi.

File UTF-8 và UTF-16 giữ nguyên encoding. File dùng bảng mã cũ (code page) bị từ chối, vì đoán sai bảng mã có thể làm hỏng file. Chạy `quill --help` để xem mọi tùy chọn.

## Tùy chọn format

`quill --style file.json` và extension (đọc file `%USERPROFILE%\.quillsql.json`) nhận một file tùy chọn nhỏ. Trường nào cũng có thể bỏ qua:

```json
{
  "layout": true,
  "indentSize": 4,
  "leadingCommas": false,
  "applyCasing": true,
  "prefixUnicode": false,
  "insertSemicolons": true,
  "encloseIdentifiers": "WithoutBrackets",
  "tableAliasStyle": "AddAs"
}
```

`encloseIdentifiers` nhận thêm giá trị `WithBrackets`; `tableAliasStyle` nhận thêm `RemoveAs`.

Muốn tùy chỉnh chi tiết hơn thì dùng **style theo nhóm**: các thiết lập được chia theo `whitespace`, `lists`, `joinStatements`, `caseExpressions`… Bạn chỉ cần ghi những giá trị khác với mặc định trong [`src/quill_defaults.json`](src/quill_defaults.json). CLI sẽ cảnh báo thiết lập nào không được áp dụng.

## File và cài đặt

| Đường dẫn | Công dụng |
|---|---|
| `%USERPROFILE%\.quillsql.json` | Tùy chọn format |
| `%USERPROFILE%\.quillsql-colors.json` | Màu tab và thanh màu theo server |
| `%LOCALAPPDATA%\QuillSQL\settings.json` | `{ "autoUpdate": false }` để tắt kiểm tra cập nhật |
| `%LOCALAPPDATA%\QuillSQL\ai.json` | Model AI và ngôn ngữ trả lời |
| `%LOCALAPPDATA%\QuillSQL\Snippets\` | Snippet của bạn |
| `%LOCALAPPDATA%\QuillSQL\history.jsonl` | Lịch sử query |
| `%LOCALAPPDATA%\QuillSQL\quillsql.log` | Log; tạo thêm file rỗng tên `verbose` cạnh nó để ghi chi tiết hơn |

## Build từ mã nguồn

Cần Rust (bản stable) và Visual Studio 2022 có MSBuild; VS SDK được tải qua NuGet.

```powershell
cargo test
.\vsix\build.ps1                 # build quill.exe và vsix\QuillSql.Vsix\bin\Release\QuillSql.Vsix.vsix
.\vsix\install.ps1               # đóng SSMS trước; -RootSuffix Exp để cài vào instance thử, -Uninstall để gỡ
```

```
src/      Engine Rust: lexer, format, IntelliSense, refactor, phân tích code, plan, snippet
tests/    Test của engine
vsix/     Extension cho SSMS: lớp vỏ C# mỏng, chạy quill.exe thành tiến trình riêng
assets/   Logo (file SVG gốc; assets\logo\render.ps1 tạo PNG và .ico)
scripts/  set-version.ps1
```

SSMS chỉ nạp package .NET Framework chạy chung tiến trình, nên extension phải viết bằng C#. Extension không chứa logic SQL nào; nó nói chuyện với engine qua pipe, nên engine có lỗi cũng không làm SSMS sập.

Phát hành bản mới:
1. Chạy `.\scripts\set-version.ps1 0.3.0`.
2. Commit.
3. Push tag `v0.3.0`.

Workflow Release sẽ chạy test, build và đăng `QuillSQL.vsix`, `quill.exe` cùng `SHA256SUMS.txt`. Các máy đã cài sẽ được đề nghị cập nhật trong vòng một ngày.

## Giới hạn hiện tại

- QuillSQL đọc T-SQL theo token chứ không phân tích cú pháp đầy đủ. Nó xử lý được các query, DML và DDL thường gặp. Với cú pháp lạ, có thể không có gợi ý, không refactor hay không cảnh báo, nhưng code không bị sửa sai.
- Với đăng nhập Microsoft Entra, các tính năng cần metadata không dùng được. Format, snippet, tô màu và lịch sử vẫn hoạt động.
- `insertSemicolons` bỏ qua những chỗ không chắc chắn, ví dụ trước `THROW`.
- Smart Rename với object trong database chỉ tạo script; nó không tự sửa các object khác.
- Tô màu tab phụ thuộc vào cấu trúc tab tài liệu của SSMS 21; một phiên bản SSMS sau này có thể cần cập nhật lại.

## Giấy phép

[MIT](LICENSE)
