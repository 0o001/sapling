/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use progress_model::CacheStats;
use progress_model::IoTimeSeries;
use progress_model::ProgressBar;
use progress_model::Registry;

use crate::RenderingConfig;

#[test]
fn test_simple_render() {
    let reg = example();
    let config = RenderingConfig::for_testing();
    assert_eq!(
        format!("\n{}", crate::simple::render(&reg, &config)),
        r#"
       Files  110 (9% miss)
       Trees  110 (9% miss)
         Net  [ ▁▁▂▂▃▃▄▄▅▅▆▆▇█]  ▼ 67KB/s  154 requests
        Disk  [ ▁▁▂▂▃▃▄▄▅▅▆▆▇█]  ▲ 4050B/s
       Files  [=======>       ]  5KB/10KB
       Trees  [     <=>       ]  5KB
     Commits  [=======>       ]  5KB/10KB
       Files  [=======>       ]  5KB/10KB  ./foo/Files/文…
       Trees  [     <=>       ]  5KB  ./foo/Trees/文件名
              and 4 more"#
    );
}

/// Example registry with some progress bars.
fn example() -> Registry {
    let reg = Registry::default();

    // Time series.
    for &(topic, unit) in &[("Net", "requests"), ("Disk", "files")] {
        let series = IoTimeSeries::new(topic, unit);
        if topic == "Net" {
            series.populate_test_samples(1, 0, 11);
        } else {
            series.populate_test_samples(0, 1, 0);
        }
        reg.register_io_time_series(&series);
    }

    // Cache stats
    for &topic in &["Files", "Trees"] {
        let stats = CacheStats::new(topic);
        stats.increase_hit(100);
        stats.increase_miss(10);
        reg.register_cache_stats(&stats);
    }

    // Progress bars
    for i in 0..3 {
        for &topic in &["Files", "Trees", "Commits"] {
            let total = if topic == "Trees" { 0 } else { 10000 };
            let bar = ProgressBar::new(topic, total, "bytes");
            bar.increase_position(5000);
            reg.register_progress_bar(&bar);
            if i == 1 {
                let message = format!("./foo/{}/文件名", topic);
                bar.set_message(message);
            }
        }
    }

    reg
}
