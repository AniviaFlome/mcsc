mod state;
mod view;
mod workers;

use rune::{AppSpec, Cx, El};
use state::App;

fn app_view(s: &App, cx: &mut Cx<App>) -> El {
    view::view(s, cx)
}

fn state_init() -> App {
    App::new()
}

fn main() {
    rune::set_app_id("mcsc");
    let app = rune::App::new(AppSpec::<App> {
        scenes: &[
            ("light", |s| {
                s.dark = false;
            }),
            ("legacy-split", |s| {
                s.target = state::Target::LegacySplit;
            }),
            ("in-place", |s| {
                s.in_place = true;
            }),
        ],
        ..AppSpec::new(
            "mcsc — Minecraft Server World Converter",
            (1152.0, 800.0),
            state_init,
            app_view,
        )
    });
    let handle = app.handle();
    let app = app.init(move || App::new_with_handle(handle));
    app.run();
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Env, Runtime};

    fn harness(dark: bool) -> Runtime<App> {
        let mut app = App::new();
        app.dark = dark;
        Runtime::new(app, app_view, (1152.0, 800.0), Env::new(dark, 1.0))
    }

    #[test]
    fn view_builds_and_exposes_key_actions() {
        let mut rt = harness(true);
        rt.frame(0.0);
        for key in [
            "root",
            "header",
            "source-card",
            "output-card",
            "target-card",
            "player-card",
            "options-card",
            "status-card",
            "server-root",
            "level-name",
            "output-dir",
            "player-name",
            "online-uuid",
            "browse",
            "browse-output",
            "detect",
            "scan",
            "convert",
            "clear",
            "theme",
            "left",
            "right",
            "footer",
        ] {
            assert!(
                rt.find(key).is_some(),
                "expected keyed element {key} in tree"
            );
        }
    }

    #[test]
    fn header_badge_is_gone() {
        // The layout pill was removed; no badge in any detection state.
        for dark in [true, false] {
            let mut rt = harness(dark);
            rt.frame(0.0);
            assert!(rt.find("layout-pill").is_none());
        }
    }

    #[test]
    fn theme_toggle_flips_dark() {
        let mut rt = harness(true);
        rt.frame(0.0);
        let (x, y) = rt.find("theme").expect("theme button");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        for _ in 0..6 {
            rt.frame(1.0 / 60.0);
        }
        assert!(!rt.state.dark);
    }

    #[test]
    fn target_buttons_switch_target() {
        let mut rt = harness(true);
        rt.frame(0.0);
        let (x, y) = rt.find("target-legacy").expect("legacy target button");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(1.0 / 60.0);
        assert_eq!(rt.state.target, state::Target::LegacySplit);
    }

    #[test]
    fn typing_server_root_mirrors_output_default() {
        let mut rt = harness(true);
        rt.frame(0.0);
        rt.update(|s, _| {
            s.server_root.text = "/tmp/mc/server".to_string();
            s.refresh_output_default();
        });
        rt.frame(1.0 / 60.0);
        assert!(
            rt.state.output_dir.text.contains("/tmp/mc"),
            "output mirrors server root, got {}",
            rt.state.output_dir.text
        );
    }
}
