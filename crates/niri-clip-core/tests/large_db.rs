//! 大库长稳测试（ROADMAP 任务 2.5）：100k 条规模下的写入 / 查询 / 并发 / 迁移 / 维护。
//!
//! **默认 `#[ignore]`**：100k 规模耗时以分钟计，不适合进 PR 门禁（门禁仍是那
//! 71 个快速用例）。手动触发：
//!
//! ```text
//! cargo test -p niri-clip-core --release --test large_db -- --ignored --nocapture
//! ```
//!
//! 规模可覆盖，便于快速自检与二分定位：
//!
//! ```text
//! NIRI_CLIP_STRESS_N=3000 cargo test -p niri-clip-core --release --test large_db -- --ignored --nocapture
//! ```
//!
//! 沙盒位置默认取系统临时目录，**大盘不够时必须用 `NIRI_CLIP_STRESS_DIR`
//! 改指**（100k 的库含 FTS5 trigram 索引约 40 MiB，两个沙盒加迁移快照 ≈ 150 MiB；
//! 本机 `/tmp` 只有 10 MiB，必踩）：
//!
//! ```text
//! NIRI_CLIP_STRESS_DIR=/path/on/big/disk cargo test -p niri-clip-core --release \
//!     --test large_db -- --ignored --nocapture
//! ```
//!
//! 对齐 ROADMAP 2.5 的三条验收标准：
//! - **无锁死**：每一步都上报耗时；外层可 `timeout` 兜底，卡住即红
//! - **无数据丢失**：每阶段都有条目数与内容断言（含迁移的"只减不增 + 精确合并数"）
//! - **内存平稳**：重复查询下 RSS 不随轮次增长；迁移峰值 RSS 单独上报
//!
//! **写入段分两个口径，别混淆**：
//! 1. **批量种子**（单事务）——把库快速做到 100k，测的是"写入 N 条"这件事本身
//! 2. **逐条 `insert_with`**（1b，200 条）——捕获热路径的真实单条成本。
//!    它每次都 `connect()` 一次，在文件创建/删除昂贵的文件系统上被 I/O 主导
//!    （本机沙盒实测 ≈10ms/条，而 `create+write+unlink` 单次就要 43ms）。
//!    因此 **1b 的绝对值只对本机文件系统有意义**，别拿它当 niri-clip 的开销基线。
//!    之所以不逐条灌 100k：那要 15 分钟以上，量的是文件系统不是程序。
//!
//! 为什么必须放在 core 的集成测试里：`v3→v4` blake3 迁移只能在**旧库**上验证，
//! 而造旧库需要照着 `migrate.rs` 的 v1/v2/v3 步骤写 schema——外壳脚本做不到，
//! 断言能力也弱。`migrate.rs` 里那句"100k 行极端规模的压力验证归任务 2.5"，
//! 指的就是这里。
//!
//! 沙盒纪律：`XDG_*` 全部指向临时目录，绝不触碰真实历史库（AGENTS.md §一）。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use niri_clip_core::config::Config;
use niri_clip_core::store;
use rusqlite::Connection;

/// ROADMAP 2.5 的验收规模
const DEFAULT_N: usize = 100_000;
/// 故意制造的重复文本组数（每组 2 行、文本相同但旧 hash 不同）——
/// 这是 2.2 迁移必须合并的形态，100k 规模下验证它仍然成立
const DUP_GROUPS: usize = 500;
/// 并发写入阶段：线程数 × 每线程条数
const CONC_THREADS: usize = 4;
const CONC_PER_THREAD: usize = 250;
/// 内存平稳观察：重复查询轮数与 RSS 采样间隔
const MEM_ROUNDS: usize = 200;
const MEM_SAMPLE_EVERY: usize = 50;
/// 迁移后抽样校验 hash 完整性的行数（全量重算一遍会额外吃一份内存，
/// 抽样足够抓住"迁移整体没跑/跑错算法"这类失败）
const HASH_SAMPLE: usize = 500;

fn scale() -> usize {
    std::env::var("NIRI_CLIP_STRESS_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_N)
}

/// 沙盒根目录：默认系统临时目录，可用 `NIRI_CLIP_STRESS_DIR`（或 `TMPDIR`）改写。
///
/// **这不是可有可无的开关**：容器/CI 里 `/tmp` 常是小容量 tmpfs（本机实测
/// 只有 10 MiB），100k 条的库加 FTS trigram 索引远超此数，会以
/// `database or disk is full` 死在写入中途。换到大盘再跑。
fn sandbox_root() -> PathBuf {
    std::env::var_os("NIRI_CLIP_STRESS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// 文件系统可用字节（statvfs）。非 unix 或调用失败返回 None → 预检降级为提示
#[cfg(unix)]
fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs 只写我们提供的这块已初始化的缓冲区
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
    if rc != 0 {
        return None;
    }
    Some(st.f_bavail as u64 * st.f_frsize as u64)
}

#[cfg(not(unix))]
fn free_bytes(_path: &Path) -> Option<u64> {
    None
}

fn human(bytes: u64) -> String {
    const MIB: f64 = 1_048_576.0;
    if bytes as f64 >= MIB {
        format!("{:.0} MiB", bytes as f64 / MIB)
    } else {
        format!("{:.1} MiB", bytes as f64 / MIB)
    }
}

/// `(db.sqlite, db.sqlite-wal)` 字节。WAL 是尚未检查点的数据载荷，必须计入
fn db_files_bytes() -> (u64, u64) {
    let p = Config::db_path();
    let size = |f: &Path| std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
    (size(&p), size(&p.with_extension("sqlite-wal")))
}

/// 独立 XDG 沙盒。每次清空重建，避免上一轮残留使断言失真
fn sandbox(tag: &str) -> PathBuf {
    let root = sandbox_root().join(format!("niri-clip-stress-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("state")).expect("create sandbox state");
    std::env::set_var("XDG_STATE_HOME", root.join("state"));
    std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
    std::env::set_var("XDG_CACHE_HOME", root.join("cache"));
    // SQLite 的临时文件（大 DELETE / ORDER BY 的临时 b-tree）默认落 /tmp，
    // 与库不在一处。`/tmp` 是小容量 tmpfs 时会以 `SQLITE_FULL` 报
    // "database or disk is full"——而磁盘明明还有几百 G，极难定位。
    // prune 在 10 万行上要为 `id IN (... ORDER BY ts)` 建临时索引，必踩。
    std::env::set_var("SQLITE_TMPDIR", &root);
    root
}

/// 长稳配置：`max_items` 必须 ≥ 规模。
///
/// 默认 750 下每次 insert 都会立刻按 ts 淘汰最旧行，100k 条根本存不下来——
/// 这不是缺陷而是配置语义。另外注意 `max_items = 0` **不是**"不限"：
/// `enforce_max_items` 会因此清空全库。
fn stress_cfg(scale: usize) -> Config {
    Config {
        max_items: scale * 2,
        ..Config::default()
    }
}

/// 种子文本：中英混合、长度与真实剪贴板同量级。
///
/// 两条约束，破坏了会得到极难定位的假失败：
/// 1. **避开默认 `ignore_regex`**（`(?i)password|secret|token|otp|auth`）——
///    命中会被 `should_ignore` 静默过滤，表现为"写不进去"
/// 2. 参与 FTS 查询的尾标记只用**字母数字**，不带 `-`/空格：trigram 分词按
///    非字母数字切分，带分隔符的 needle 会被拆成多 token，短语匹配易落空
fn seed_text(i: usize) -> String {
    format!("longrun-entry-{i:06} 这是一段用于长稳测试的中英文混合剪贴板内容 payload{i:07}")
}

/// 唯一可检索标记（与 seed_text 的尾标记同源）
fn seed_needle(i: usize) -> String {
    format!("payload{i:07}")
}

/// 捕获热路径探针的文本（逐条走 `insert_with`）。同样避开默认 `ignore_regex`
fn capture_text(i: usize) -> String {
    format!("capture-probe-{i:05} 捕获热路径单条写入 probe payload{i:05}")
}

/// 重复组的文本（每组两行共用同一串，仅旧 hash 不同）
fn dup_text(g: usize) -> String {
    format!("longrun-dup-{g:06} 重复文本用于验证迁移合并 duppayload{g:06}x")
}

/// 重复组的唯一可检索标记
fn dup_needle(g: usize) -> String {
    format!("duppayload{g:06}x")
}

/// (`VmRSS`, `VmHWM`) KiB。只有 Linux 有 `/proc`；其他平台返回 None，
/// 内存断言随之降级为"跳过并说明"而非误报失败
fn rss_kib() -> Option<(u64, u64)> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let pick = |key: &str| -> Option<u64> {
        s.lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse().ok())
    };
    Some((pick("VmRSS:")?, pick("VmHWM:")?))
}

fn rss_mib() -> Option<f64> {
    rss_kib().map(|(rss, _)| rss as f64 / 1024.0)
}

fn open_db() -> Connection {
    Connection::open(Config::db_path()).expect("open stress db")
}

fn row_count() -> i64 {
    open_db()
        .query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))
        .expect("count clips")
}

fn pinned_count() -> i64 {
    open_db()
        .query_row("SELECT COUNT(*) FROM clips WHERE pinned=1", [], |r| {
            r.get(0)
        })
        .expect("count pinned")
}

fn count_with_hash(hash: &str) -> i64 {
    open_db()
        .query_row(
            "SELECT COUNT(*) FROM clips WHERE hash=?1",
            rusqlite::params![hash],
            |r| r.get(0),
        )
        .expect("count by hash")
}

fn db_user_version() -> i64 {
    open_db()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("user_version")
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn hdr(s: &str) {
    println!("\n──── {s} ────");
}

struct LegacyFixture {
    /// 旧库总行数
    before: i64,
    /// 迁移后应有的行数（每组重复合并掉 1 行）
    after: i64,
    survivor_old_hash: String,
    survivor_text: String,
}

/// 造一个 `user_version=3` 的旧库（等价于 2.2 迁移落地前的用户库）。
///
/// schema 逐字照抄 `migrate.rs` 的 v1/v2/v3 三步：这是"模拟已有旧库"的正当
/// 做法——被测对象正是"旧库能否无损升到 v4"，构造它的过程本就不该复用新
/// 代码（否则等于自己验证自己）。
fn build_v3_legacy(n_unique: usize, dup_groups: usize) -> LegacyFixture {
    // store::connect 会自建父目录，这里绕开它直接开库，得自己建
    let db = Config::db_path();
    std::fs::create_dir_all(db.parent().expect("db parent")).expect("create legacy db dir");
    let conn = open_db();
    conn.execute_batch(
        "
        CREATE TABLE clips (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            hash TEXT UNIQUE,
            text TEXT NOT NULL,
            mime TEXT DEFAULT 'text/plain',
            ts INTEGER NOT NULL,
            pinned INTEGER DEFAULT 0,
            size INTEGER
        );
        CREATE INDEX idx_hash ON clips(hash);
        CREATE INDEX idx_pinned_ts ON clips(pinned DESC, ts DESC);
        ALTER TABLE clips ADD COLUMN image_path TEXT;
        CREATE VIRTUAL TABLE clips_fts USING fts5(
            text, content='clips', content_rowid='id', tokenize='trigram'
        );
        CREATE TRIGGER clips_fts_ai AFTER INSERT ON clips BEGIN
            INSERT INTO clips_fts(rowid, text) VALUES (new.id, new.text);
        END;
        CREATE TRIGGER clips_fts_ad AFTER DELETE ON clips BEGIN
            INSERT INTO clips_fts(clips_fts, rowid, text)
            VALUES ('delete', old.id, old.text);
        END;
        CREATE TRIGGER clips_fts_au AFTER UPDATE OF text ON clips BEGIN
            INSERT INTO clips_fts(clips_fts, rowid, text)
            VALUES ('delete', old.id, old.text);
            INSERT INTO clips_fts(rowid, text) VALUES (new.id, new.text);
        END;
        PRAGMA user_version=3;
        ",
    )
    .expect("build v3 schema");

    // 旧 hash 用 legacy-* / dup-old-* 标记：迁移会按 blake3 整体重算，旧值
    // 本身无关紧要，UNIQUE 约束只要求它们互不相同
    let base_ts = 1_700_000_000_000i64;
    conn.execute_batch("BEGIN").expect("begin seed");
    {
        let mut stmt = conn
            .prepare("INSERT INTO clips (hash, text, ts, size, pinned) VALUES (?1, ?2, ?3, ?4, ?5)")
            .expect("prepare seed");
        for i in 0..n_unique {
            let t = seed_text(i);
            stmt.execute(rusqlite::params![
                format!("legacy-{i:07}"),
                &t,
                base_ts + i as i64,
                t.len() as i64,
                0
            ])
            .expect("seed unique row");
        }
        // 第 g 组两行同文本、旧 hash 不同（a 较旧、b 较新 → 幸存 b）。
        // 第 0 组的**较旧行**额外打星标，用来验证合并时 pinned 取 OR
        for g in 0..dup_groups {
            let t = dup_text(g);
            let ts = base_ts + n_unique as i64 + g as i64 * 2;
            let pinned_a = i64::from(g == 0);
            stmt.execute(rusqlite::params![
                format!("dup-old-{g:04}-a"),
                &t,
                ts,
                t.len() as i64,
                pinned_a
            ])
            .expect("seed dup row a");
            stmt.execute(rusqlite::params![
                format!("dup-old-{g:04}-b"),
                &t,
                ts + 1,
                t.len() as i64,
                0
            ])
            .expect("seed dup row b");
        }
    }
    conn.execute_batch("COMMIT").expect("commit seed");

    // ▶ 指针指向第 0 组幸存行（旧 hash）——迁移后必须重映射到 blake3
    let survivor_old_hash = format!("dup-old-{:04}-b", 0);
    store::touch_current(&survivor_old_hash);

    LegacyFixture {
        before: row_count(),
        after: (n_unique + dup_groups) as i64,
        survivor_old_hash,
        survivor_text: dup_text(0),
    }
}

/// 沙盒根目录下的可用空间下限。写入段的库 + FTS trigram 索引是主要开销，
/// 空间不足会在写入中途以 `SQLITE_FULL` 死掉而看不出真因，故前置拦住
const MIN_FREE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[test]
#[ignore = "100k 规模长稳测试，手动触发：cargo test -p niri-clip-core --release --test large_db -- --ignored --nocapture"]
fn large_db_long_stability() {
    let n = scale();
    let cfg = stress_cfg(n);
    let t_total = Instant::now();
    println!("=== niri-clip 大库长稳测试：N={n}（DUP_GROUPS={DUP_GROUPS}）===");

    // 空间预检：宁可在开头明确拦住，也不要写到一半 SQLITE_FULL
    let root = sandbox_root();
    std::fs::create_dir_all(&root).expect("create sandbox root");
    match free_bytes(&root) {
        Some(free) => {
            println!("沙盒根目录 {}（可用 {}）", root.display(), human(free));
            assert!(
                free >= MIN_FREE_BYTES,
                "沙盒所在盘可用空间不足：{} < {}。请用 NIRI_CLIP_STRESS_DIR 指向大盘\n（本机 /tmp 是 10 MiB tmpfs，必然不够）",
                human(free),
                human(MIN_FREE_BYTES)
            );
        }
        None => println!("沙盒根目录 {}（可用空间未知，跳过预检）", root.display()),
    }

    // ───────────────────────── 1. 写入 ─────────────────────────
    hdr("1. 写入");
    sandbox("write");
    // 1a. 批量种子：**先经公开入口建库**（schema 走真实迁移路径，不由测试
    //     自建），再单事务批量灌入。为什么不像基准那样逐条走 insert_with：
    //     `insert_with` 每次调用都 `connect()` 一次（open + PRAGMA + 迁移检查
    //     + drop），在文件创建/删除昂贵的文件系统上单条成本被 I/O 主导——
    //     本机沙盒实测 ≈10ms/条，而纯粹 `create+write+unlink` 一次就要 43ms，
    //     量出来的根本不是 niri-clip 的开销。逐条路径的真实成本在 1b 单独测。
    let _ = store::list(1).expect("init schema via public path");
    let base_ts = 1_700_000_000_000i64;
    let t = Instant::now();
    {
        let conn = open_db();
        conn.execute_batch("BEGIN").expect("begin bulk seed");
        {
            let mut stmt = conn
                .prepare(
                    "INSERT INTO clips (hash, text, ts, size, pinned) VALUES (?1, ?2, ?3, ?4, 0)",
                )
                .expect("prepare bulk seed");
            for i in 0..n {
                let txt = seed_text(i);
                stmt.execute(rusqlite::params![
                    store::hash_text(&txt),
                    &txt,
                    base_ts + i as i64,
                    txt.len() as i64
                ])
                .expect("bulk seed row");
            }
        }
        conn.execute_batch("COMMIT").expect("commit bulk seed");
    }
    let bulk_elapsed = t.elapsed();
    // 无数据丢失：批量种子后条目数必须精确等于规模
    assert_eq!(
        row_count(),
        n as i64,
        "批量写入后条目数应与规模一致，无静默淘汰"
    );
    let (db_b, wal_b) = db_files_bytes();
    println!(
        "批量种子 {n} 条：{:.1}s（{:.2}ms/条）；库体积 {}（db {} + wal {}）",
        bulk_elapsed.as_secs_f64(),
        ms(bulk_elapsed) / n as f64,
        human(db_b + wal_b),
        human(db_b),
        human(wal_b)
    );

    // 1b. 逐条 API 写入：捕获热路径在 100k 库上的真实单条成本（含 connect）
    let probe_rows = 200usize;
    let t = Instant::now();
    let mut current_expected = String::new();
    for i in 0..probe_rows {
        let text = capture_text(i);
        assert!(
            store::insert_with(text.clone(), None, &cfg).expect("api insert"),
            "第 {i} 条捕获探针应新插入"
        );
        current_expected = text;
    }
    let api_ms = ms(t.elapsed()) / probe_rows as f64;
    assert_eq!(
        row_count(),
        (n + probe_rows) as i64,
        "逐条 API 写入不得丢条"
    );
    // 只做"无锁死"兜底；真实成本随文件系统差异大，不设性能阈值
    assert!(api_ms < 500.0, "单条捕获成本异常：{api_ms:.1}ms");
    println!(
        "逐条捕获 {probe_rows} 条：{api_ms:.2}ms/条（含每次 connect；文件系统慢时此项被 I/O 主导）"
    );
    if let Some(m) = rss_mib() {
        println!("．  写入后 RSS {m:.1} MiB");
    }

    // ───────────────────────── 2. 查询 ─────────────────────────
    hdr("2. 查询");
    let t = Instant::now();
    let rows = store::list(store::MENU_LIMIT).expect("list");
    let list_ms = ms(t.elapsed());
    assert_eq!(rows.len(), store::MENU_LIMIT, "list 应取满窗口");
    // 当前项语义在 100k 规模下不塌：▶ 仍是最后写入的那条
    assert_eq!(
        rows[0].text, current_expected,
        "第 1 行应为 ▶ 当前项（最后写入者）"
    );

    let mid = n / 2;
    let t = Instant::now();
    let hits = store::search(&seed_needle(mid), store::SEARCH_LIMIT).expect("fts search");
    let fts_ms = ms(t.elapsed());
    assert!(
        hits.iter().any(|c| c.text == seed_text(mid)),
        "FTS 应命中 mid 条目 {}",
        seed_needle(mid)
    );

    // <3 字符走 LIKE 全库回退：100k × 长文本下这是最贵的一条路径，必须上报
    let t = Instant::now();
    let short_hits = store::search("长稳", store::SEARCH_LIMIT).expect("like search");
    let like_ms = ms(t.elapsed());
    assert!(!short_hits.is_empty(), "短查询应命中");

    let t = Instant::now();
    let st = store::stats().expect("stats");
    let stats_ms = ms(t.elapsed());
    assert_eq!(st.total, (n + probe_rows) as i64, "stats 条数应与写入一致");

    // 上界只做"无锁死/无 O(n²)"的兜底（宽松，避免慢机器误红）；预算级阈值
    // （list <11ms、搜索 <50ms）由 1.6 的 criterion 基准在 CI 把关
    assert!(list_ms < 500.0, "list 耗时应无异常：{list_ms:.1}ms");
    assert!(fts_ms < 1000.0, "FTS 搜索耗时应无异常：{fts_ms:.1}ms");
    assert!(like_ms < 5000.0, "LIKE 回退耗时应无异常：{like_ms:.1}ms");
    println!(
        "list({}) {list_ms:.2}ms ／ FTS 搜索 {fts_ms:.2}ms ／ LIKE 回退 {like_ms:.2}ms ／ stats {stats_ms:.2}ms",
        store::MENU_LIMIT
    );

    // ───────────────────────── 3. 并发写入 ─────────────────────────
    hdr("3. 并发写入（多连接争抢写锁）");
    let before_conc = row_count();
    let failures = std::sync::Mutex::new(Vec::<String>::new());
    let t = Instant::now();
    std::thread::scope(|s| {
        for t_id in 0..CONC_THREADS {
            let cfg = &cfg;
            let failures = &failures;
            s.spawn(move || {
                for i in 0..CONC_PER_THREAD {
                    let text = format!("conc-{t_id}-{i:05} 并发写入内容 concurrent payload");
                    match store::insert_with(text, None, cfg) {
                        Ok(true) => {}
                        Ok(false) => failures
                            .lock()
                            .unwrap()
                            .push(format!("t{t_id}#{i} 被误判重复或被过滤")),
                        Err(e) => failures.lock().unwrap().push(format!("t{t_id}#{i} {e}")),
                    }
                }
            });
        }
    });
    let conc_elapsed = t.elapsed();
    let expected_conc = (CONC_THREADS * CONC_PER_THREAD) as i64;
    let fails = failures.into_inner().unwrap();
    assert!(
        fails.is_empty(),
        "并发写入出现失败（锁死/超时/丢写）：{fails:?}"
    );
    assert_eq!(
        row_count(),
        before_conc + expected_conc,
        "并发写入不得丢数据"
    );
    println!(
        "{} 线程 × {} 条：{:.1}s，无失败，条目数 +{expected_conc} 校验通过",
        CONC_THREADS,
        CONC_PER_THREAD,
        conc_elapsed.as_secs_f64()
    );

    // ───────────────────────── 4. 内存平稳 ─────────────────────────
    hdr("4. 内存平稳（重复操作下的 RSS）");
    let mut samples: Vec<f64> = Vec::new();
    for r in 0..MEM_ROUNDS {
        let _ = store::list(store::MENU_LIMIT).expect("mem list");
        let _ = store::search("longrun", store::SEARCH_LIMIT).expect("mem search");
        if r % MEM_SAMPLE_EVERY == 0 {
            if let Some(m) = rss_mib() {
                samples.push(m);
            }
        }
    }
    match (samples.first().copied(), samples.last().copied()) {
        (Some(first), Some(last)) => {
            let grow = last - first;
            println!(
                "{} 轮 list+search：RSS {first:.1} → {last:.1} MiB（增长 {grow:+.1} MiB）",
                MEM_ROUNDS
            );
            // "平稳"的判据：不随轮次单调膨胀。给足分配器抖动余量
            assert!(
                grow < 32.0,
                "重复操作下 RSS 增长 {grow:.1} MiB，疑似泄漏或缓存无界"
            );
        }
        _ => println!("非 Linux：跳过 RSS 断言"),
    }

    // ───────────────────────── 5. 维护命令 ─────────────────────────
    hdr("5. 维护（prune 保护语义 / vacuum）");
    // 显式星标两条（跳过第 1 行的当前项），让保护语义断言有实际内容
    let sample = store::list(5).expect("sample for pin");
    for c in sample.iter().skip(1).take(2) {
        assert!(store::toggle_pin(c.id).expect("pin"), "应变为已星标");
    }
    assert_eq!(pinned_count(), 2, "应已星标两条");

    // 让"全部条目都过期"：cutoff 取必然大于所有 ts 的值。这样删掉的就该恰好
    // 是"总数 − 星标 − 当前项"，保护语义一眼可验
    let cutoff = i64::MAX;
    let total_before = row_count();
    let pinned_before = pinned_count();
    let cur_hash = store::current_hash().expect("应存在当前项指针");
    let cur_rows = count_with_hash(&cur_hash);
    assert_eq!(cur_rows, 1, "当前项指针应指向一条真实存在的条目");

    let dry = store::prune_before(cutoff, true).expect("prune dry-run");
    assert_eq!(row_count(), total_before, "--dry-run 不得动任何数据");
    assert_eq!(
        dry.deleted as i64,
        total_before - pinned_before - cur_rows,
        "dry-run 预测的删除数应等于（总数 − 星标 − 当前项）"
    );
    let real = store::prune_before(cutoff, false).expect("prune");
    assert_eq!(real.deleted, dry.deleted, "dry-run 口径应与实际一致");
    assert_eq!(
        row_count(),
        pinned_before + cur_rows,
        "仅星标与当前项应存活"
    );
    assert_eq!(pinned_count(), pinned_before, "星标条目不得被删");
    assert_eq!(count_with_hash(&cur_hash), 1, "当前项不得被删");

    let (vb, va) = store::vacuum().expect("vacuum");
    assert!(va <= vb, "vacuum 后体积不应增大");
    println!(
        "prune 删 {} 条（dry-run 预测 {} 条，一致）；星标 {pinned_before} + 当前项 {cur_rows} 存活\n．  vacuum：{:.1} MiB → {:.1} MiB",
        real.deleted,
        dry.deleted,
        vb as f64 / 1_048_576.0,
        va as f64 / 1_048_576.0
    );

    // ───────────────────────── 6. v3 → v4 blake3 迁移 ─────────────────────────
    hdr("6. v3 → v4 blake3 迁移");
    sandbox("migrate");
    let fx = build_v3_legacy(n, DUP_GROUPS);
    assert_eq!(fx.before, fx.after + DUP_GROUPS as i64, "旧库构造口径自检");
    assert_eq!(db_user_version(), 3, "构造出的应是 v3 旧库");
    let rss_pre = rss_mib();

    // 触发迁移：任意公开入口都会经 connect() → migrate_schema()
    let t = Instant::now();
    let st = store::stats().expect("trigger migration");
    let mig_elapsed = t.elapsed();
    let rss_peak = rss_kib().map(|(_, hwm)| hwm as f64 / 1024.0);

    assert_eq!(db_user_version(), 4, "迁移后 user_version 应为 4");
    assert_eq!(
        st.total, fx.after,
        "迁移后条目数应为 {}（合并 {DUP_GROUPS} 组重复），实为 {}",
        fx.after, st.total
    );
    assert!(
        st.total <= fx.before,
        "条目数只减不增：迁移前 {} → 后 {}",
        fx.before,
        st.total
    );

    // ▶ 指针重映射：旧 hash 已不存在，必须已换成新 hash
    let cur = store::current_hash().expect("迁移后指针应存在");
    assert_ne!(cur, fx.survivor_old_hash, "指针应已重映射");
    assert_eq!(
        cur,
        store::hash_text(&fx.survivor_text),
        "指针应指向幸存行的 blake3 指纹"
    );

    let conn = open_db();
    // 指纹全局唯一（重复行已合并；UNIQUE 也会兜底，这里显式断言语义）
    let dup_hashes: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM (SELECT hash FROM clips GROUP BY hash HAVING COUNT(*) > 1)",
            [],
            |r| r.get(0),
        )
        .expect("dup hash count");
    assert_eq!(dup_hashes, 0, "迁移后不应残留重复指纹");

    // 全量重算抽检：hash 必须等于 blake3(text)
    let mut checked = 0usize;
    {
        let mut stmt = conn
            .prepare("SELECT hash, text FROM clips WHERE hash NOT LIKE 'img:%' LIMIT ?1")
            .expect("prepare hash check");
        let it = stmt
            .query_map(rusqlite::params![HASH_SAMPLE as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .expect("query hash check");
        for row in it {
            let (h, txt) = row.expect("hash row");
            assert_eq!(h, store::hash_text(&txt), "抽检行的 hash 应为 blake3(text)");
            checked += 1;
        }
    }
    assert!(checked > 0, "抽检不应为空");

    // 合并语义：第 0 组较旧行带星标 → 幸存行应继承（pinned 取 OR）
    let survivor_pinned: i64 = conn
        .query_row(
            "SELECT pinned FROM clips WHERE hash=?1",
            rusqlite::params![store::hash_text(&fx.survivor_text)],
            |r| r.get(0),
        )
        .expect("survivor pinned");
    assert_eq!(
        survivor_pinned, 1,
        "合并后应继承被并行上的星标（pinned 取 OR）"
    );

    // FTS 与内容表一致性：迁移的 DELETE 走触发器，索引必须同步
    let hits = store::search(&dup_needle(0), 10).expect("fts after migration");
    assert!(
        hits.iter().any(|c| c.text == fx.survivor_text),
        "迁移后 FTS 应仍能命中幸存行"
    );

    // 快照必须落盘（迁移的唯一退路）
    let snap = Config::state_dir().join("db.sqlite.pre-blake3");
    assert!(snap.exists(), "迁移前快照应存在：{}", snap.display());
    let snap_b = std::fs::metadata(&snap).map(|m| m.len()).unwrap_or(0);
    let (mig_db, mig_wal) = db_files_bytes();
    println!(
        "．  迁移后库体积 {}（db {} + wal {}），快照 {}",
        human(mig_db + mig_wal),
        human(mig_db),
        human(mig_wal),
        human(snap_b)
    );

    println!(
        "迁移：{:.1}s，条目 {} → {}（合并 {DUP_GROUPS} 组重复）\n．  指针已重映射，抽检 {checked} 行 hash 全对，FTS 命中正常\n．  快照 {}",
        mig_elapsed.as_secs_f64(),
        fx.before,
        st.total,
        snap.display()
    );
    if let (Some(pre), Some(peak)) = (rss_pre, rss_peak) {
        println!("．  迁移前 RSS {pre:.1} MiB → 进程峰值 {peak:.1} MiB");
    }

    println!(
        "\n=== 长稳测试通过：总耗时 {:.1}s ===",
        t_total.elapsed().as_secs_f64()
    );
}
