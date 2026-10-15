//! 应用级定时 Pull：只处理当前仓库；任务归应用所有，不依赖分支窗口是否打开。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use gpui_kit::*;

use super::AutoPullResult;
use crate::config::{AutoPullConfig, DEFAULT_AUTO_PULL_INTERVAL_SECS, RepoConfig, current_repo};
use crate::repo::git_get_repo;
use crate::status::ui::StatusTreeHandle;

#[derive(Default)]
struct Schedule {
    active: Option<(PathBuf, Duration, Instant)>,
}

impl Schedule {
    fn poll(&mut self, plan: Option<(PathBuf, Duration)>, now: Instant) -> Option<PathBuf> {
        let Some((path, interval)) = plan else {
            self.active = None;
            return None;
        };
        match self.active.as_mut() {
            Some((previous, period, next)) if *previous == path && *period == interval => {
                if now >= *next {
                    *next = now + interval;
                    Some(path)
                } else {
                    None
                }
            }
            _ => {
                // 首次开启、换仓库或改周期都重新计时，不立即操作工作区。
                self.active = Some((path, interval, now + interval));
                None
            }
        }
    }

    fn finished(&mut self, now: Instant) {
        if let Some((_, interval, next)) = self.active.as_mut() {
            // 耗时超过周期时也不补跑：下一次从完成时算起，防止失败/慢网络重试风暴。
            *next = now + *interval;
        }
    }

    fn next(&self) -> Option<Instant> {
        self.active.as_ref().map(|(_, _, next)| *next)
    }
}

#[derive(Default)]
pub(crate) struct AutoPull {
    pub path: Option<PathBuf>,
    pub running: bool,
    pub next: Option<Instant>,
    pub last_result: Option<std::result::Result<String, String>>,
    pub revision: u64,
    _task: Option<Task<()>>,
}
impl Global for AutoPull {}

fn read_plan() -> Result<(PathBuf, AutoPullConfig)> {
    let repo = git_get_repo(current_repo()?)?;
    let path = repo
        .workdir()
        .context("当前仓库没有工作目录")?
        .canonicalize()?;
    let settings = RepoConfig::load(&path)?.auto_pull;
    if settings.enabled {
        settings
            .interval()
            .with_context(|| format!("{} 的自动 Pull 周期无效", path.display()))?;
    }
    Ok((path, settings))
}

/// 启动一次。关闭窗口不停止；应用退出时 Task 被 drop，不再发起下一次 Pull。
pub(crate) fn init(cx: &mut App) {
    if cx.has_global::<AutoPull>() {
        return;
    }
    cx.set_global(AutoPull::default());
    let task = cx.spawn(async move |cx| {
        let mut schedule = Schedule::default();
        loop {
            // ponytail: 每秒读一次当前仓库的小配置；配置 I/O 成为瓶颈时改成事件通知。
            cx.background_executor().timer(Duration::from_secs(1)).await;
            if cx.try_read_global::<AutoPull, _>(|_, _| ()).is_none() {
                return;
            }
            let plan = cx.background_executor().spawn(async { read_plan() }).await;
            let (path, settings) = match plan {
                Ok(plan) => plan,
                Err(err) => {
                    schedule.poll(None, Instant::now());
                    cx.update_global::<AutoPull, _>(|auto, _| {
                        auto.path = None;
                        auto.next = None;
                        auto.last_result = Some(Err(format!("定时计划无法读取：{err:#}")));
                    });
                    continue;
                }
            };
            let period = Duration::from_secs(
                settings
                    .interval()
                    .unwrap_or(DEFAULT_AUTO_PULL_INTERVAL_SECS),
            );
            let due = schedule.poll(
                settings.enabled.then(|| (path.clone(), period)),
                Instant::now(),
            );
            cx.update_global::<AutoPull, _>(|auto, _| {
                if auto.path.as_ref() != Some(&path) {
                    auto.last_result = None;
                }
                auto.path = Some(path.clone());
                auto.next = schedule.next();
            });
            let Some(path) = due else { continue };
            cx.update_global::<AutoPull, _>(|auto, _| {
                auto.running = true;
                auto.next = None;
            });
            let job_path = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    // 到点后重新检查一次，避免排队期间换仓库/停用仍然开始 Pull。
                    let (current, settings) = read_plan()?;
                    if current != job_path
                        || !settings.enabled
                        || Duration::from_secs(settings.interval()?) != period
                    {
                        return Ok(AutoPullResult::Skipped("当前仓库或周期设置已改变"));
                    }
                    super::try_auto_pull(&job_path)
                })
                .await;
            let pulled = matches!(result, Ok(AutoPullResult::Pulled(_)));
            let message = match result {
                Ok(AutoPullResult::Pulled(message)) => Ok(format!("自动 Pull 完成：{message}")),
                Ok(AutoPullResult::Skipped(reason)) => Ok(format!("本次跳过：{reason}")),
                Err(err) => Err(format!("自动 Pull 失败：{err:#}")),
            };
            schedule.finished(Instant::now());
            cx.update_global::<AutoPull, _>(|auto, _| {
                auto.running = false;
                auto.next = schedule.next();
                auto.last_result = Some(message);
                auto.revision = auto.revision.saturating_add(1);
            });
            if pulled {
                cx.update(|cx| {
                    if current_repo()
                        .is_ok_and(|current| current.canonicalize().ok().as_ref() == Some(&path))
                        && let Some(handle) = cx.try_global::<StatusTreeHandle>().cloned()
                    {
                        handle.refresh(cx);
                    }
                });
            }
        }
    });
    cx.global_mut::<AutoPull>()._task = Some(task);
}

pub(crate) fn running(cx: &App) -> bool {
    cx.try_global::<AutoPull>().is_some_and(|auto| auto.running)
}

pub(crate) fn status(cx: &App, path: &Path) -> String {
    let Some(auto) = cx.try_global::<AutoPull>() else {
        return "定时任务尚未启动".to_string();
    };
    if auto.path.is_none()
        && let Some(Err(message)) = &auto.last_result
    {
        return message.clone();
    }
    if auto.path.as_deref() != Some(path) {
        return "暂停：定时 Pull 只对主窗口当前打开的仓库生效".to_string();
    }
    let mut text = if auto.running {
        "自动 Pull 正在执行…".to_string()
    } else if let Some(next) = auto.next {
        format!(
            "下次自动 Pull 约 {} 秒后",
            next.saturating_duration_since(Instant::now()).as_secs()
        )
    } else {
        "自动 Pull 已关闭".to_string()
    };
    if let Some(result) = &auto.last_result {
        text.push('\n');
        text.push_str(match result {
            Ok(message) | Err(message) => message,
        });
    }
    text
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use super::Schedule;
    use crate::config::{AutoPullConfig, RepoConfig};

    #[test]
    fn schedule_resets_on_period_repo_and_enable_changes_without_catchup() {
        let mut schedule = Schedule::default();
        let now = Instant::now();
        let a = PathBuf::from("repo-a");
        let b = PathBuf::from("repo-b");
        let period = Duration::from_secs(10);
        assert_eq!(schedule.poll(Some((a.clone(), period)), now), None);
        assert_eq!(
            schedule.poll(Some((a.clone(), period)), now + Duration::from_secs(9)),
            None
        );
        assert_eq!(
            schedule.poll(Some((a.clone(), period)), now + period),
            Some(a.clone())
        );
        schedule.finished(now + Duration::from_secs(50));
        assert_eq!(
            schedule.poll(Some((a.clone(), period)), now + Duration::from_secs(51)),
            None
        );
        assert_eq!(
            schedule.poll(Some((a.clone(), period)), now + Duration::from_secs(60)),
            Some(a.clone())
        );
        let changed = Duration::from_secs(30);
        assert_eq!(
            schedule.poll(Some((a, changed)), now + Duration::from_secs(61)),
            None
        );
        assert_eq!(schedule.next(), Some(now + Duration::from_secs(91)));
        assert_eq!(
            schedule.poll(Some((b.clone(), changed)), now + Duration::from_secs(62)),
            None
        );
        assert_eq!(schedule.next(), Some(now + Duration::from_secs(92)));
        assert_eq!(schedule.poll(None, now + Duration::from_secs(63)), None);
        assert_eq!(schedule.next(), None);
        assert_eq!(
            schedule.poll(Some((b.clone(), changed)), now + Duration::from_secs(100)),
            None
        );
        assert_eq!(
            schedule.poll(Some((b.clone(), changed)), now + Duration::from_secs(130)),
            Some(b)
        );
    }

    #[test]
    fn interval_validation_and_old_config_defaults_are_safe() {
        let old: RepoConfig =
            toml::from_str("author = 'Test'\nemail = 'test@example.com'").unwrap();
        assert!(!old.auto_pull.enabled);
        assert_eq!(old.auto_pull.interval().unwrap(), 300);
        for text in [
            "0",
            "9",
            "86401",
            "-1",
            "1.5",
            "",
            "abc",
            "18446744073709551616",
        ] {
            assert!(AutoPullConfig::parse_interval(text).is_err(), "{text}");
        }
        for seconds in [10, 300, 86_400] {
            assert_eq!(
                AutoPullConfig::parse_interval(&seconds.to_string()).unwrap(),
                seconds
            );
        }
        let restored: RepoConfig = toml::from_str(&toml::to_string(&old).unwrap()).unwrap();
        assert_eq!(restored, old);
    }
}
