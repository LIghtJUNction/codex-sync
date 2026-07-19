use super::{DoctorReport, Report};
use std::collections::BTreeMap;

pub fn print_report(report: &Report) {
    println!("当前 provider：{}", report.current_provider);
    println!(
        "当前 model：{}",
        report.current_model.as_deref().unwrap_or("(未配置)")
    );
    println!("数据库线程：{}", report.total_threads);
    print_counts("数据库 provider 分布", &report.database_counts);
    if !report.model_counts.is_empty() {
        print_counts("数据库 model 分布", &report.model_counts);
    }
    print_counts("会话文件 provider 分布", &report.rollout_counts);
    if !report.rollout_model_counts.is_empty() {
        print_counts("会话文件 model 分布", &report.rollout_model_counts);
    }
    println!(
        "待归并：{} 条数据库记录，{} / {} 个会话文件，{} 条重复 session_meta，缺少 {} 条侧栏索引，残留 {} 条侧栏索引，缺失 {} 个会话文件",
        report.database_rows_to_change,
        report.rollout_files_to_change,
        report.rollout_files,
        report.duplicate_session_meta_lines,
        report.missing_session_index_entries,
        report.stale_session_index_entries,
        report.missing_rollout_files
    );
}

pub fn print_doctor_report(report: &DoctorReport) {
    println!(
        "数据库完整性：{}",
        if report.database_integrity_ok {
            "正常"
        } else {
            "异常"
        }
    );
    if !report.database_integrity_ok {
        for message in &report.database_integrity_messages {
            println!("  {message}");
        }
    }
    println!(
        "线程：数据库 {}，应显示于侧栏 {}；会话文件 {}",
        report.total_threads, report.sidebar_threads, report.rollout_files
    );
    println!(
        "会话异常：非法 JSONL {}，缺少 session_meta {}，重复 session_meta {}，孤立文件 {}，缺失文件 {}",
        report.invalid_rollout_files,
        report.missing_session_meta_files,
        report.duplicate_session_meta_lines,
        report.orphan_rollout_files,
        report.missing_rollout_files
    );
    println!(
        "索引异常：非法行 {}，重复 ID {}，缺少 {}，残留 {}",
        report.invalid_session_index_lines,
        report.duplicate_session_index_ids,
        report.missing_session_index_entries,
        report.stale_session_index_entries
    );
    println!(
        "诊断结论：{}",
        if report.clean {
            "干净"
        } else {
            "需要处理"
        }
    );
    if report.repaired_session_index {
        println!("已原子修复侧栏索引");
    }
    if report.restored_rollout_files != 0 {
        println!("已原子恢复 rollout：{}", report.restored_rollout_files);
    }
    if report.deduplicated_session_meta_files != 0 {
        println!(
            "已原子去重 session_meta 文件：{}",
            report.deduplicated_session_meta_files
        );
    }
    if let Some(backup) = &report.backup {
        println!("修改前数据备份：{}", backup.display());
    }
}

fn print_counts(label: &str, counts: &BTreeMap<String, usize>) {
    println!("{label}：");
    for (name, count) in counts {
        println!("  {name}: {count}");
    }
}
