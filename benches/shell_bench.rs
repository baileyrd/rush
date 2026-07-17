//! Throughput benchmarks for rush's hot parsing/expansion/evaluation
//! paths — the same components `fuzz/` exercises for correctness, here
//! for performance instead, so a future change to the lexer, parser,
//! arithmetic evaluator, or glob matcher has a number to check against
//! rather than "feels slower."
//!
//! Deliberately narrow, same reasoning as `fuzz/`: every benchmark here
//! is pure (no process spawn, no filesystem I/O beyond what `parser::parse`
//! itself needs), so `cargo bench` stays fast and deterministic. Variable
//! expansion is benchmarked with plain `$VAR`/`${VAR}` forms only — no
//! `$(...)` command substitution, which would spawn a real subprocess per
//! iteration and swamp the numbers with fork/exec overhead rather than
//! measuring rush's own expansion code.

use criterion::{Criterion, black_box, criterion_group, criterion_main};

fn bench_lex_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("lex_parse");

    let short = "echo hello world";
    group.bench_function("short_pipeline", |b| {
        b.iter(|| rush::parser::parse(black_box(short)))
    });

    let script = r#"
        for i in 1 2 3 4 5; do
            if [ "$i" -gt 2 ]; then
                echo "big: $i" | grep -v foo | wc -l
            else
                echo "small: $i"
            fi
        done
        case "$1" in
            foo|bar) echo matched ;;
            *) echo default ;;
        esac
        while read -r line; do
            printf '%s\n' "$line"
        done < "$file"
    "#;
    group.bench_function("multi_construct_script", |b| {
        b.iter(|| rush::parser::parse(black_box(script)))
    });

    group.finish();
}

fn bench_arith(c: &mut Criterion) {
    let mut group = c.benchmark_group("arith_eval");

    group.bench_function("simple_expr", |b| {
        b.iter(|| rush::arith::eval(black_box("1 + 2 * 3")))
    });

    group.bench_function("nested_ternary_and_bitwise", |b| {
        b.iter(|| {
            rush::arith::eval(black_box(
                "((1 << 4) | (2 & 3)) ? (10 % 3) + (5 ** 2) : (7 / 2 - 1)",
            ))
        })
    });

    group.finish();
}

fn bench_glob_match(c: &mut Criterion) {
    let mut group = c.benchmark_group("glob_match");

    group.bench_function("simple_star", |b| {
        b.iter(|| rush::glob::match_component(black_box("*.rs"), black_box("main.rs")))
    });

    group.bench_function("extglob_alternation", |b| {
        b.iter(|| {
            rush::glob::match_component(
                black_box("@(foo|bar|baz)*.txt"),
                black_box("bazqux.txt"),
            )
        })
    });

    group.bench_function("bracket_class", |b| {
        b.iter(|| {
            rush::glob::match_component(black_box("[[:alpha:]][[:digit:]]*"), black_box("a1b2c3"))
        })
    });

    group.finish();
}

fn bench_variable_expansion(c: &mut Criterion) {
    rush::vars::set("FOO", "hello");
    rush::vars::set("BAR", "world");
    rush::vars::set("PATH_LIKE", "/usr/local/bin:/usr/bin:/bin");

    let mut group = c.benchmark_group("expand_dollars");

    group.bench_function("plain_vars", |b| {
        b.iter(|| rush::expand::expand_dollars(black_box("$FOO $BAR ${FOO}_${BAR}")))
    });

    group.bench_function("parameter_expansion_operators", |b| {
        b.iter(|| {
            rush::expand::expand_dollars(black_box(
                "${PATH_LIKE%%:*} ${PATH_LIKE##*:} ${FOO:-default} ${FOO^^}",
            ))
        })
    });

    group.finish();
}

criterion_group!(benches, bench_lex_parse, bench_arith, bench_glob_match, bench_variable_expansion);
criterion_main!(benches);
