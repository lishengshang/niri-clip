//! store 基准（路线图 1.6）：10k 条库上的 `list(300)` 与裸 sqlite 查询。
//!
//! 预算门禁（ROADMAP 开销预算表）：10k list < 11ms、sqlite 10k 查询 < 4ms；
//! CI 后续对基准耗时做 >20% 回归报警。XDG 环境隔离进临时目录，不触碰真实历史库。

use std::path::PathBuf;
use std::sync::OnceLock;

use criterion::{criterion_group, criterion_main, Criterion};
use niri_clip_core::{config::Config, store};
use rusqlite::Connection;

const SEED_ROWS: usize = 10_000;

/// 每进程一次性搭建：临时 XDG 环境 + 10k 条种子数据。种子经公开入库 API
/// 写入（与真实捕获路径同构，不复制 schema 内部细节，schema 演进不破坏基准）；
/// 各基准共用同一份只读库。bench 无 teardown 钩子，临时目录留给系统清理。
fn bench_env() -> &'static PathBuf {
    static ENV: OnceLock<PathBuf> = OnceLock::new();
    ENV.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("niri-clip-bench-{}", std::process::id()));
        std::fs::create_dir_all(root.join("state")).unwrap();
        std::env::set_var("XDG_STATE_HOME", root.join("state"));
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_CACHE_HOME", root.join("cache"));
        // row_count 预检不经 store::connect（后者会自建父目录），需先建好
        std::fs::create_dir_all(Config::db_path().parent().unwrap()).unwrap();

        // 幂等：进程重启复用同名目录（pid 回绕）时跳过重播种
        if row_count() < SEED_ROWS as i64 {
            let cfg = Config::load();
            for i in 0..SEED_ROWS {
                let text = format!("bench-entry-{i:05} 这是一段用于基准的中英文混合 payload text");
                store::insert_with(text, None, &cfg).expect("seed insert");
            }
        }
        root
    })
}

fn row_count() -> i64 {
    let conn = Connection::open(Config::db_path()).unwrap();
    conn.query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))
        .unwrap_or(0)
}

/// 端到端 list：含 Config::load 与 connect（open + PRAGMA + 迁移检查）——
/// 与 TUI/GUI 每次取数的真实开销同口径，对应预算表「10k 条 list」。
fn bench_list(c: &mut Criterion) {
    bench_env();
    store::list(300).expect("warmup");
    c.bench_function("list_300_of_10k", |b| b.iter(|| store::list(300).unwrap()));
}

/// 裸 sqlite 查询：连接常开，隔离出纯查询+取行成本，对应预算表
/// 「sqlite 查询」。SELECT * 对列集演进免疫（数值含全部列物化，偏保守）。
fn bench_sqlite(c: &mut Criterion) {
    bench_env();
    let conn = Connection::open(Config::db_path()).unwrap();
    c.bench_function("sqlite_select_300_of_10k", |b| {
        b.iter(|| {
            let mut stmt = conn
                .prepare("SELECT * FROM clips ORDER BY ts DESC, id DESC LIMIT 300")
                .unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, i64>(0)).unwrap();
            let n = rows.filter_map(Result::ok).count();
            assert_eq!(n, 300);
        })
    });
}

criterion_group!(benches, bench_list, bench_sqlite);
criterion_main!(benches);
