use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// 目录级隔离 + 串行化：通过 XDG_* 环境变量把所有持久化位置指进临时目录，
/// 测试互不影响且不会触碰真实用户目录。锁用全局共享的 test_util::ENV_LOCK
/// （store/config/tui 三处测试必须互斥，见 lib.rs 注释）
use crate::test_util::ENV_LOCK;
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct EnvGuard {
    prev_state: Option<String>,
    prev_config: Option<String>,
    prev_cache: Option<String>,
    root: PathBuf,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        restore_var("XDG_STATE_HOME", &self.prev_state);
        restore_var("XDG_CONFIG_HOME", &self.prev_config);
        restore_var("XDG_CACHE_HOME", &self.prev_cache);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn restore_var(key: &str, val: &Option<String>) {
    match val {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
}

fn with_env(f: impl FnOnce(&EnvGuard)) {
    let _g = ENV_LOCK.lock().unwrap();
    let seq = SEQ.fetch_add(1, Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!("niri-clip-ut-{}-{}", std::process::id(), seq));
    std::fs::create_dir_all(root.join("state")).unwrap();

    let prev_state = std::env::var("XDG_STATE_HOME").ok();
    let prev_config = std::env::var("XDG_CONFIG_HOME").ok();
    let prev_cache = std::env::var("XDG_CACHE_HOME").ok();
    std::env::set_var("XDG_STATE_HOME", root.join("state"));
    std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
    std::env::set_var("XDG_CACHE_HOME", root.join("cache"));

    let guard = EnvGuard {
        prev_state,
        prev_config,
        prev_cache,
        root,
    };
    f(&guard);
    drop(guard);
}

fn clear_db() {
    let p = Config::db_path();
    let _ = std::fs::remove_file(&p);
    let _ = std::fs::remove_file(p.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(p.with_extension("sqlite-shm"));
}

#[test]
fn should_ignore_filters_secrets_and_short_input() {
    let cfg = Config::default();
    assert!(should_ignore("my password is hunter2", &cfg));
    assert!(should_ignore("", &cfg), "空文本应被忽略");
    let cfg2 = Config {
        min_store_length: 5,
        ..Config::default()
    };
    assert!(should_ignore("ab", &cfg2));
    assert!(!should_ignore("hello world", &cfg));
    assert!(!should_ignore("plain text", &cfg));
}

#[test]
fn upsert_dedups_same_hash_atomically() {
    with_env(|_| {
        clear_db();
        assert!(insert("dup-entry-a".into(), None).unwrap());
        assert!(!insert("dup-entry-a".into(), None).unwrap());
        let all = list(100).unwrap();
        let hits = all
            .iter()
            .filter(|c| c.text.contains("dup-entry-a"))
            .count();
        assert_eq!(hits, 1, "相同文本应只占一行");
    });
}

#[test]
fn insert_trims_whitespace_and_dedups_variants() {
    with_env(|_| {
        clear_db();
        // 带首尾空白与纯净版视为同一 hash：消除 watch 管道与
        // try_system_capture 的 trim 语义分歧导致的孪生条目
        assert!(insert("dup-entry-x\n".into(), None).unwrap());
        assert!(!insert("dup-entry-x".into(), None).unwrap());
        assert!(!insert("  dup-entry-x  ".into(), None).unwrap());
        let all = list(100).unwrap();
        let hits = all.iter().filter(|c| c.text == "dup-entry-x").count();
        assert_eq!(hits, 1, "空白变体应只占一行且入库为 trim 后文本");
        // 纯空白不入库
        assert!(!insert("   \n\t ".into(), None).unwrap());
    });
}

#[test]
fn busy_timeout_is_set_on_connection() {
    with_env(|_| {
        clear_db();
        let conn = connect().unwrap();
        let v: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, BUSY_TIMEOUT_MS as i64);
        let uv: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(uv, 4, "schema 应迁移到版本 4（FTS5 + blake3 统一）");
    });
}

#[test]
fn image_insert_associates_data_file_and_dedups_by_content_not_length() {
    with_env(|_| {
        clear_db();
        let a = b"\x89PNG\r\n\x1a\n bytes-of-A ".to_vec();
        let img_a = insert_image("image/png", &a)
            .unwrap()
            .expect("first insert");
        let clip_a = get(img_a.id).unwrap();
        assert_eq!(
            clip_a.image_path.as_deref(),
            Some(img_a.path.to_string_lossy().as_ref()),
            "clip 行应记录自身数据文件路径"
        );
        assert!(img_a.path.exists());

        // 相同内容重复拷贝 -> 判重，仅刷时间戳
        let again = insert_image("image/png", &a).unwrap();
        assert!(again.is_none());

        // 相同字节长度但内容不同 -> 不得再因 len 判重（修复点）
        let mut b = a.clone();
        b[10] ^= 0xFF;
        let img_b = insert_image("image/png", &b)
            .unwrap()
            .expect("len equal but different bytes");
        assert_ne!(img_b.id, img_a.id);

        let conn = Connection::open(Config::db_path()).unwrap();
        let total: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM clips WHERE mime LIKE 'image/%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(total, 2);
    });
}

#[test]
fn insert_rejects_oversize_text_at_boundary() {
    with_env(|g| {
        clear_db();
        // 通过测试隔离环境写入小限额配置，Config::load() 在 insert 内生效
        let cfg_dir = g.root.join("config/niri-clip");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("config.toml"), "max_clip_bytes = 64\n").unwrap();

        let at_limit = "x".repeat(64);
        assert!(insert(at_limit, None).unwrap(), "恰好达到上限应入库");
        let over = "y".repeat(65);
        assert!(!insert(over, None).unwrap(), "超限一个字节即拒绝");

        let all = list(10).unwrap();
        assert_eq!(all.len(), 1, "超限条目不得落库");
        assert!(all[0].text.starts_with('x'));
    });
}

#[test]
fn insert_image_rejects_oversize_payload_at_boundary() {
    with_env(|g| {
        clear_db();
        let cfg_dir = g.root.join("config/niri-clip");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("config.toml"), "max_image_bytes = 64\n").unwrap();

        let big = vec![0u8; 65];
        assert!(
            insert_image("image/png", &big).unwrap().is_none(),
            "超限图片应拒绝且不产生数据文件"
        );
        assert!(!Config::images_dir().join("1.bin").exists());

        let ok = vec![0u8; 64];
        let img = insert_image("image/png", &ok)
            .unwrap()
            .expect("恰好达到上限应入库");
        assert!(img.path.exists());
    });
}

#[test]
fn current_pointer_tracks_capture_and_tops_list() {
    with_env(|_| {
        clear_db();
        insert("old-a".into(), None).unwrap();
        insert("old-b".into(), None).unwrap();
        assert!(current_hash().is_some(), "insert 成功即写指针");
        let all = list(10).unwrap();
        assert_eq!(all[0].text, "old-b", "最后捕获者置顶");

        // 星标压不过当前项：当前项永远第 1 行
        let pinned_id = all[1].id; // old-a
        toggle_pin(pinned_id).unwrap();
        assert_eq!(list(10).unwrap()[0].text, "old-b", "星标不得顶掉当前项");

        // 重复捕获（dedup 刷 ts 路径）同样刷新指针
        insert("old-a".into(), None).unwrap();
        let cur = current_hash().unwrap();
        assert_eq!(list(10).unwrap()[0].hash, cur, "▶ 应跟随最后一次捕获");
    });
}

#[test]
fn oversize_or_ignored_capture_does_not_move_current_pointer() {
    with_env(|g| {
        clear_db();
        insert("keep-me".into(), None).unwrap();
        let cur = current_hash().unwrap();

        // 超限拒绝不写指针
        let cfg_dir = g.root.join("config/niri-clip");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("config.toml"), "max_clip_bytes = 8\n").unwrap();
        insert("this is way beyond eight bytes".into(), None).unwrap();
        assert_eq!(current_hash().unwrap(), cur, "超限捕获不得移动 ▶");
        // ignore_regex 命中不写指针
        insert("my password is hunter2".into(), None).unwrap();
        assert_eq!(current_hash().unwrap(), cur, "被过滤捕获不得移动 ▶");
    });
}

#[test]
fn legacy_cache_db_is_snapshotted_into_state_dir() {
    with_env(|g| {
        // 在旧的 ~/.cache 位置构造一个含数据的历史库
        let legacy = Config::legacy_db_path();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        {
            let lc = Connection::open(&legacy).unwrap();
            lc.execute_batch(
                "CREATE TABLE clips (id INTEGER PRIMARY KEY AUTOINCREMENT, hash TEXT UNIQUE,
                 text TEXT NOT NULL, mime TEXT DEFAULT 'text/plain', ts INTEGER NOT NULL,
                 pinned INTEGER DEFAULT 0, size INTEGER);
                 INSERT INTO clips(hash, text, ts) VALUES ('legacy-1','old entry',1);",
            )
            .unwrap();
        }
        // 环境已把新状态目录指向临时区，任何一次 connect() 都应触发搬迁
        assert!(insert("new entry".into(), None).unwrap());
        let all = list(50).unwrap();
        assert!(
            all.iter().any(|c| c.text == "old entry"),
            "旧库条目应出现在迁移后的新库中"
        );
        assert!(
            Config::db_path().exists(),
            "新库应位于 {:?}",
            g.root.join("state")
        );
    });
}

#[test]
fn pin_orders_first_and_list_respects_limit() {
    with_env(|_| {
        clear_db();
        for i in 0..5 {
            insert(format!("item-{i}"), None).unwrap();
        }
        let all = list(TUI_LIMIT).unwrap();
        let head_id = all[0].id;
        toggle_pin(head_id).unwrap();
        let after = list(TUI_LIMIT).unwrap();
        assert_eq!(after[0].id, head_id, "pinned 应置顶");
        assert!(after[0].pinned);
        let few = list(3).unwrap();
        assert_eq!(few.len(), 3);
    });
}

#[test]
fn delete_and_wipe_remove_image_files() {
    with_env(|_| {
        clear_db();
        let a = insert_image("image/png", b"\x89PNG-a")
            .unwrap()
            .expect("insert a");
        let b = insert_image("image/png", b"\x89PNG-b")
            .unwrap()
            .expect("insert b");
        assert!(a.path.exists() && b.path.exists());
        delete(a.id).unwrap();
        assert!(!a.path.exists(), "delete 应同步删除数据文件");
        assert!(b.path.exists());
        wipe().unwrap();
        assert!(!b.path.exists(), "wipe 应清空所有数据文件");
    });
}

#[test]
fn max_items_eviction_removes_image_files() {
    with_env(|g| {
        clear_db();
        let cfg_dir = g.root.join("config/niri-clip");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("config.toml"), "max_items = 1\n").unwrap();
        let a = insert_image("image/png", b"\x89PNG-a")
            .unwrap()
            .expect("insert a");
        let b = insert_image("image/png", b"\x89PNG-b")
            .unwrap()
            .expect("insert b");
        // b 入库触发淘汰：a（未 pin、更旧）连行带文件一起消失
        assert!(get(a.id).is_err(), "淘汰条目的行应被删除");
        assert!(!a.path.exists(), "淘汰条目的数据文件应被删除");
        assert!(b.path.exists());
    });
}

#[test]
fn prune_orphan_images_removes_unreferenced_files() {
    with_env(|_| {
        clear_db();
        let img = insert_image("image/png", b"\x89PNG-ok")
            .unwrap()
            .expect("insert");
        let dir = Config::images_dir();
        // 旧版本遗留的孤儿：文件在、行不在
        let orphan = dir.join("9999.bin");
        std::fs::write(&orphan, b"orphan").unwrap();
        // 入库中途崩溃的临时文件
        let tmp = dir.join(".tmp-1234.bin");
        std::fs::write(&tmp, b"tmp").unwrap();
        assert_eq!(prune_orphan_images().unwrap(), 2);
        assert!(!orphan.exists() && !tmp.exists());
        assert!(img.path.exists(), "被引用的文件不得误删");
        assert_eq!(prune_orphan_images().unwrap(), 0, "二次清扫应无残留");
    });
}

#[test]
fn gc_images_evicts_oldest_first_and_protects_pinned_and_current() {
    with_env(|_| {
        clear_db();
        // 三张 10 字节小图，间隔毫秒保证 ts 有序（淘汰按 ts ASC, id ASC）
        let a = insert_image("image/png", &[7u8; 10]).unwrap().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(3));
        let b = insert_image("image/png", &[8u8; 10]).unwrap().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(3));
        let c = insert_image("image/png", &[9u8; 10]).unwrap().unwrap();

        // b 星标、c 当前项，均应受保护
        let conn = connect().unwrap();
        conn.execute("UPDATE clips SET pinned=1 WHERE id=?1", params![b.id])
            .unwrap();
        drop(conn);
        assert_eq!(
            current_hash().unwrap(),
            image_content_key("image/png", &[9u8; 10]),
            "c 刚入库应为当前项"
        );

        // 总量 30、配额 25：需释放 ≥5 字节 → 淘汰最旧且未受保护的 a
        assert_eq!(gc_images(25).unwrap(), 1);
        assert!(!a.path.exists(), "被淘汰条目的数据文件应被删除");
        assert!(b.path.exists() && c.path.exists(), "星标/当前项不得误删");
        let conn = connect().unwrap();
        let cnt: i64 = conn
            .query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cnt, 2);
    });
}

#[test]
fn gc_images_zero_means_unlimited_and_within_quota_is_noop() {
    with_env(|_| {
        clear_db();
        insert_image("image/png", &[1u8; 10]).unwrap().unwrap();
        // 0 = 不限制；配额内不动任何条目
        assert_eq!(gc_images(0).unwrap(), 0);
        assert_eq!(gc_images(1024).unwrap(), 0);
    });
}

#[test]
fn fts_migration_backfills_and_upgrade_is_lossless() {
    with_env(|_| {
        clear_db();
        // 手工构造 v2 旧库文件（不经过 connect()，否则直接迁满）
        {
            // 裸开不会自建父目录（connect 才会）
            std::fs::create_dir_all(Config::db_path().parent().unwrap()).unwrap();
            let conn = Connection::open(Config::db_path()).unwrap();
            conn.execute_batch(
                "CREATE TABLE clips (id INTEGER PRIMARY KEY AUTOINCREMENT, hash TEXT UNIQUE,
                 text TEXT NOT NULL, mime TEXT DEFAULT 'text/plain', ts INTEGER NOT NULL,
                 pinned INTEGER DEFAULT 0, size INTEGER, image_path TEXT);
                 INSERT INTO clips(hash, text, mime, ts) VALUES
                 ('h1', 'hello legacy world', 'text/plain', 1),
                 ('h2', '旧库存量中文条目', 'text/plain', 2);
                 PRAGMA user_version=2;",
            )
            .unwrap();
        }
        // 下一次 connect() 走 v3 迁移：回填后存量行可搜（中英文）
        assert_eq!(search("legacy world", 10).unwrap().len(), 1);
        assert_eq!(search("存量中文", 10).unwrap().len(), 1);
        let n: i64 = connect()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "旧库升级无损：行数不变");
    });
}

#[test]
fn fts_stays_in_sync_with_insert_and_delete() {
    with_env(|_| {
        clear_db();
        insert("the quick brown fox".into(), None).unwrap();
        insert("剪贴板历史管理器条目".into(), None).unwrap();
        assert_eq!(search("quick brown", 10).unwrap().len(), 1);
        let hit = search("板历史", 10).unwrap().pop().unwrap();
        // delete 行删 → 触发器同步出 FTS 索引
        delete(hit.id).unwrap();
        assert!(search("板历史", 10).unwrap().is_empty());
        assert_eq!(search("quick brown", 10).unwrap().len(), 1);
    });
}

#[test]
fn search_short_query_falls_back_to_like_and_escapes_wildcards() {
    with_env(|_| {
        clear_db();
        insert("100% pure path example".into(), None).unwrap();
        insert("ab filler".into(), None).unwrap();
        // <3 字符走 LIKE；通配符按字面匹配（% _ 不展开）
        assert_eq!(search("ab", 10).unwrap().len(), 1);
        assert_eq!(search("%", 10).unwrap().len(), 1, "% 应按字面匹配");
        assert_eq!(
            search("_", 10).unwrap().len(),
            0,
            "_ 不作通配符且数据无字面 _"
        );
        assert!(search("", 10).unwrap().is_empty());
    });
}

#[test]
fn search_match_phrase_syntax_from_user_input_is_safe() {
    with_env(|_| {
        clear_db();
        insert("quoted \"inner\" text".into(), None).unwrap();
        // 双引号在 FTS 查询语法里有含义：翻倍转义后按字面命中且不 panic
        assert_eq!(search("\"inner\"", 10).unwrap().len(), 1);
        assert_eq!(search("inner", 10).unwrap().len(), 1);
        // trigram 按字面索引标点：跨引号的 "inner text" 不命中（符合子串语义）
        assert_eq!(search("inner text", 10).unwrap().len(), 0);
    });
}

/// 任务 2.2（v3→v4）：防翻倍断言。手工构造含"同文本不同 legacy hash"
/// 的 v3 旧库（DefaultHasher 跨编译器不稳定的真实翻倍形态），锁定：
/// 条目数只减不增、文本行全量重算、图片指纹不动、FTS 同步、指针重映射、
/// 幂等、快照落盘（ROADMAP 风险表：迁移出错致数据翻倍/丢失）
#[test]
fn blake3_migration_merges_duplicates_and_never_doubles() {
    with_env(|_| {
        clear_db();
        // 裸开手工建 v3 schema（不经 connect()，否则直接迁满）
        std::fs::create_dir_all(Config::db_path().parent().unwrap()).unwrap();
        let conn = Connection::open(Config::db_path()).unwrap();
        conn.execute_batch(
            "CREATE TABLE clips (id INTEGER PRIMARY KEY AUTOINCREMENT, hash TEXT UNIQUE,
             text TEXT NOT NULL, mime TEXT DEFAULT 'text/plain', ts INTEGER NOT NULL,
             pinned INTEGER DEFAULT 0, size INTEGER, image_path TEXT);
             CREATE VIRTUAL TABLE clips_fts USING fts5(
                 text, content='clips', content_rowid='id', tokenize='trigram');
             CREATE TRIGGER clips_fts_ai AFTER INSERT ON clips BEGIN
                 INSERT INTO clips_fts(rowid, text) VALUES (new.id, new.text); END;
             CREATE TRIGGER clips_fts_ad AFTER DELETE ON clips BEGIN
                 INSERT INTO clips_fts(clips_fts, rowid, text)
                 VALUES ('delete', old.id, old.text); END;
             CREATE TRIGGER clips_fts_au AFTER UPDATE OF text ON clips BEGIN
                 INSERT INTO clips_fts(clips_fts, rowid, text)
                 VALUES ('delete', old.id, old.text);
                 INSERT INTO clips_fts(rowid, text) VALUES (new.id, new.text); END;
             INSERT INTO clips(hash, text, mime, ts, pinned) VALUES
                 ('legacy-a', 'duplicated text', 'text/plain', 100, 1),
                 ('legacy-b', 'duplicated text', 'text/plain', 200, 0),
                 ('legacy-c', 'unique text', 'text/plain', 300, 0),
                 ('legacy-d', '中文存量条目', 'text/plain', 400, 0);
             INSERT INTO clips(hash, text, mime, ts, size) VALUES
                 ('img:image/png:abc-10', '[image image/png 10 bytes]', 'image/png', 500, 10);
             PRAGMA user_version=3;",
        )
        .unwrap();
        // ▶ 指针指向将被合并掉的 legacy-a：迁移后必须重映射到幸存行
        std::fs::create_dir_all(Config::state_dir()).unwrap();
        std::fs::write(Config::state_dir().join("current"), "legacy-a").unwrap();
        drop(conn);

        let conn = connect().unwrap();
        let ver: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ver, 4, "迁移必须推进到 v4");

        // 防翻倍断言：5 行（4 文本 + 1 图片）合并 1 条重复 → 4 行，只减不增
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 4, "合并重复后条目数只减不增");

        // 幸存行 = ts 最大那份（legacy-b），hash 重算为 blake3 且星标取 OR
        let (hash, pinned): (String, i64) = conn
            .query_row(
                "SELECT hash, pinned FROM clips WHERE text='duplicated text'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            hash,
            hash_text("duplicated text"),
            "文本 hash 必须重算为 blake3"
        );
        assert_eq!(pinned, 1, "任一重复行被星标则合并后保留星标");

        // 全部文本行 hash 与 blake3(text) 一致；图片指纹原样不动
        let mut stmt = conn
            .prepare("SELECT hash, text FROM clips WHERE mime='text/plain'")
            .unwrap();
        let mut rows = stmt.query([]).unwrap();
        while let Some(r) = rows.next().unwrap() {
            let h: String = r.get(0).unwrap();
            let t: String = r.get(1).unwrap();
            assert_eq!(h, hash_text(&t), "所有文本行均须重算");
        }
        drop(rows);
        drop(stmt);
        let img: String = conn
            .query_row("SELECT hash FROM clips WHERE mime='image/png'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(img, "img:image/png:abc-10", "图片 FNV 指纹不走 blake3");

        // FTS 由触发器自动同步：被并行出索引，幸存行中英文均可搜
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM clips_fts WHERE clips_fts MATCH 'duplicated'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1, "被并行已从 FTS 删除，幸存行可搜");
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM clips_fts WHERE clips_fts MATCH '中文存量'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1, "中文行迁移后仍可搜");

        // ▶ 指针重映射到幸存行新 hash
        let cur = std::fs::read_to_string(Config::state_dir().join("current")).unwrap();
        assert_eq!(cur.trim(), hash, "指针必须重映射，否则 ▶ 静默失效");

        // 快照必须落盘（事后回滚保险）
        assert!(
            Config::state_dir().join("db.sqlite.pre-blake3").exists(),
            "迁移前 VACUUM INTO 快照必须存在"
        );

        // 幂等：二次 connect 不重复迁移、不再翻倍
        drop(conn);
        let conn = connect().unwrap();
        let n2: i64 = conn
            .query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n2, 4, "二次 connect 幂等");
    });
}
