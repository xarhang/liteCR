use litecr_infrastructure::{connect, migrate};
use std::collections::BTreeSet;

#[tokio::test]
async fn inline_tolerant_prefilter_preserves_exact_helper_positives() {
    let url = std::env::var("LITECR_TEST_DATABASE_URL")
        .expect("dedicated synthetic litecr_test database required");
    let pool = connect(&url, 1).await.expect("synthetic owner connects");
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .expect("read fixture database name");
    assert_eq!(database, "litecr_test", "never use a non-fixture database");
    migrate(&pool).await.expect("current migrations apply");
    let mut tx = pool.begin().await.expect("isolated property transaction");
    sqlx::query("SET LOCAL statement_timeout='30s'")
        .execute(&mut *tx)
        .await
        .expect("bound property query execution");

    // Test the actual runtime condition, not a separately copied implementation.
    // Its SQL contains no Rust string escapes besides continuation newlines.
    let source = include_str!("../src/matching/mod.rs");
    let begin = "/* lossless_prefilter_begin */";
    let end = "/* lossless_prefilter_end */";
    assert_eq!(source.matches(begin).count(), 1);
    assert_eq!(source.matches(end).count(), 1);
    let predicate = source
        .split_once(begin)
        .unwrap()
        .1
        .split_once(end)
        .unwrap()
        .0
        .replace("\\\r\n", " ")
        .replace("\\\n", " ");

    let controls = [
        "",
        "a",
        "ab",
        "abc",
        "abcd",
        "abcdefghijklmnop",
        "abcdefghijklmnopqrstuvwx",
        "zzzzzzzzzzzzzzzzzzzzzzzz",
        "synthetic sample abcdefghijklmnop",
        "synthetic sample abcdxefghijklmnop",
        "abcdefghijklmnop abc def",
        "abc def abcdefghijklmnop",
        "abcdefghi abc def",
        "abcdefgh abc def",
        "abcdefghi abc abc",
        "abc abc abcdefghi",
        "abcdefgh abc abc",
        "abcdefghi abc def ghi jkl mno pqr stu",
        "abcdefghi abc def ghi jkl mno pqr stu vwx",
        "abcdefgh abc def ghi jkl mno pqr stu",
        "ສົມຊາຍສີວິໄລວຽງຈັນ",
        "ສົມຊາຍສີວິໄລ ວຽງຈັນ",
        "สมชายศรีวิไลเวียงจันทน์",
        "สมชายศรีวิไล เวียงจันทน์",
        "李明王伟张丽陈芳赵强刘洋周勇吴军",
        "éabcdefghijklmno",
        "éabcdefghijklmno",
        r"abc%def_ghi\jklmnop",
        "abcXdefYghiZjklmnop",
        "%%%%%%%%%_________\\\\\\\\\\\\\\\\\\",
        "aaaaaaaaabbbbbbbbbccccccccc",
        "aaaaaaaaabbbbbbbbccccccccc",
        "aaaaaaaaa bbbbbbbbb aaaaaaaaa",
        "bbbbbbbbb aaaaaaaaa aaaaaaaaa",
        "aaaaaaaaa bbbbbbbbb",
        "aaaaaaaa bbbbbbbb",
        "abc  abcdefghijklmnop",
    ];
    let mut pairs = BTreeSet::new();
    for left in controls {
        for right in controls {
            pairs.insert((left.to_owned(), right.to_owned()));
        }
    }
    // Directed mutations force positives near the edit budget, including
    // end-position edits that can damage the selected suffix trigrams.
    for base in [
        "abcdefghijklmnopqrstuvwx",
        "synthetic sample abcdefghijklmnop",
        "ສົມຊາຍສີວິໄລວຽງຈັນ",
        "สมชายศรีวิไลเวียงจันทน์",
        "李明王伟张丽陈芳赵强刘洋周勇吴军",
        r"abc%def_ghi\jklmnop",
    ] {
        let chars: Vec<char> = base.chars().collect();
        for index in 0..chars.len() {
            let mut deleted = chars.clone();
            deleted.remove(index);
            insert_both(&mut pairs, base, deleted.into_iter().collect());
            let mut substituted = chars.clone();
            substituted[index] = 'Ω';
            insert_both(&mut pairs, base, substituted.into_iter().collect());
            let mut inserted = chars.clone();
            inserted.insert(index, '界');
            insert_both(&mut pairs, base, inserted.into_iter().collect());
            if index + 1 < chars.len() {
                let mut transposed = chars.clone();
                transposed.swap(index, index + 1);
                insert_both(&mut pairs, base, transposed.into_iter().collect());
                let mut two_substitutions = chars.clone();
                two_substitutions[index] = 'Ω';
                two_substitutions[index + 1] = '界';
                insert_both(&mut pairs, base, two_substitutions.into_iter().collect());
            }
        }
        insert_both(&mut pairs, base, format!("{base}界"));
    }
    assert!(pairs.len() < 10_000, "keep property work strictly bounded");
    let (queries, candidates): (Vec<String>, Vec<String>) = pairs.into_iter().unzip();
    let query = format!(
        "WITH pairs AS MATERIALIZED ( \
           SELECT q,c FROM unnest($1::text[],$2::text[]) pair(q,c) \
         ), evaluated AS MATERIALIZED ( \
           SELECT q,c, \
             (matching.name_edit_within_v1(q,c, \
                CASE WHEN least(char_length(q),char_length(c))>=8 THEN 2 ELSE 1 END) \
              OR matching.name_parts_within_v1(q,c) \
              OR matching.name_parts_within_v1(c,q)) IS TRUE AS helper_positive, \
             COALESCE(({predicate}),FALSE) AS admitted \
           FROM pairs \
           CROSS JOIN LATERAL ( \
             SELECT q AS normalized_key,char_length(q) AS name_length, \
               cardinality(string_to_array(q,' ')) AS part_count, \
               (SELECT value FROM unnest(string_to_array(q,' ')) part(value) \
                ORDER BY char_length(value) DESC,value LIMIT 1) AS longest_part \
           ) qa \
           CROSS JOIN LATERAL (SELECT c AS normalized_key) ca \
         ) SELECT count(*),count(*) FILTER (WHERE helper_positive), \
           count(*) FILTER (WHERE NOT admitted), \
           count(*) FILTER (WHERE helper_positive AND NOT admitted), \
           COALESCE(array_agg(q||' => '||c) \
             FILTER (WHERE helper_positive AND NOT admitted),ARRAY[]::text[]) \
         FROM evaluated"
    );
    let (total, positives, rejected, lost, examples): (i64, i64, i64, i64, Vec<String>) =
        sqlx::query_as(&query)
            .bind(queries)
            .bind(candidates)
            .fetch_one(&mut *tx)
            .await
            .expect("bounded actual-prefilter implication property");
    assert!(total > 1_000);
    assert!(
        positives > 100,
        "property must exercise real helper positives"
    );
    assert!(
        rejected > 10,
        "property must exercise the rejecting fast path"
    );
    assert_eq!(
        lost, 0,
        "prefilter lost helper-positive pairs: {examples:?}"
    );
    tx.rollback()
        .await
        .expect("remove all property session state");
    pool.close().await;
}

fn insert_both(pairs: &mut BTreeSet<(String, String)>, base: &str, variant: String) {
    pairs.insert((base.to_owned(), variant.clone()));
    pairs.insert((variant, base.to_owned()));
}
