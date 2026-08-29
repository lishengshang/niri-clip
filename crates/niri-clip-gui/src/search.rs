//! fzf 风格模糊匹配：子序列评分与命中标记。
//!
//! 设计要点：
//! - 评分匹配与高亮标记共享 ci_eq 大小写语义（ASCII 归一，中文直等）
//! - t 侧逐字符比较，免逐条 `to_lowercase` 分配（全库 750 条每键过滤）

/// fzf 风格简易评分：连续命中 +4、词首/行首 +5、基础命中 +2、
/// 位置弱惩罚。None = 子序列不匹配。
/// q 需已 lowercase；t 逐字符 ci_eq 比较（免逐条 to_lowercase 分配）
pub fn fuzzy_score(q: &str, t: &str) -> Option<i32> {
    let mut qc = q.chars();
    let mut want = qc.next();
    let mut score = 0i32;
    let mut prev_hit = false;
    let mut prev_ch: Option<char> = None;
    for (i, ch) in t.chars().enumerate() {
        if want.is_some_and(|w| ci_eq(w, ch)) {
            want = qc.next();
            score += 2;
            if prev_hit {
                score += 4; // 连续命中：词组感
            }
            if prev_ch.is_none_or(|p| !p.is_alphanumeric()) {
                score += 5; // 词首/行首
            }
            score -= (i as i32) / 64; // 位置弱惩罚
            prev_hit = true;
            if want.is_none() {
                return Some(score);
            }
        } else {
            prev_hit = false;
        }
        prev_ch = Some(ch);
    }
    None
}

/// 同 fuzzy_score 的贪心口径，但返回 t 每个字符是否命中
/// （Vec 与 t.chars() 对齐，供命中高亮分段着色）。
/// 查询未耗尽 → None（整行不高亮）。
pub fn fuzzy_flags(q: &str, t: &str) -> Option<Vec<bool>> {
    let mut qc = q.chars();
    let mut next = qc.next();
    let mut flags = Vec::with_capacity(t.chars().count());
    for ch in t.chars() {
        let hit = next.is_some_and(|n| ci_eq(n, ch));
        if hit {
            next = qc.next();
        }
        flags.push(hit);
    }
    next.is_none().then_some(flags)
}

/// 大小写不敏感比较：ASCII 归一，非 ASCII（中文等本身无大小写）直等
#[inline]
fn ci_eq(a: char, b: char) -> bool {
    a == b || a.eq_ignore_ascii_case(&b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_ranks_consecutive_and_word_start_higher() {
        // 词首连续命中应优于零散命中
        let good = fuzzy_score("ab", "a b ab");
        let scattered = fuzzy_score("ab", "xaxb");
        assert!(good.is_some() && scattered.is_some());
        assert!(good.unwrap() > scattered.unwrap());
    }

    #[test]
    fn score_rejects_non_subsequence() {
        assert!(fuzzy_score("ac", "abcx ba").is_some());
        assert!(fuzzy_score("az", "abcx ba").is_none());
    }

    #[test]
    fn flags_align_with_chars_and_case_insensitive() {
        let flags = fuzzy_flags("ab", "xAbc").unwrap();
        assert_eq!(flags, vec![false, true, true, false]);
        assert!(fuzzy_flags("az", "xAbc").is_none());
    }
}
