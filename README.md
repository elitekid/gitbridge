# gitbridge-rs

GitBridge(서버에 바로 닿지 않는 PC 와 git 번들로 주고받는 도구)의 Rust판이다. 파이썬판 1.0.5를 대체하며 첫 정식판은 2.0.0이다.

- 대상: Windows(Git for Windows, WSL 경로 저장소 포함)
- 빌드: 인터넷이 되는 PC에서 한다. 다른 PC에는 빌드한 exe만 옮긴다.
- 작업 방식: main 기준 작업트리의 feature 브랜치에서 작업한다.

## 사용

```
GitBridge export  --repo P --kind base|standard|all [--branch B] [--full] --out DIR
GitBridge create  --repo P --branch B --out DIR [--full]
GitBridge refresh --repo P
GitBridge apply   --bundle F --repo P [--sync-all]
GitBridge restore --bundle F --dest P
GitBridge queue   --dir D --src-root P [--apply]
GitBridge undo list --repo P
GitBridge undo run  --repo P [--id 기록이름]
```

- 결과는 한국어 한 줄로 내고 `--json` 이면 JSON 으로 낸다. 성공 0, 멈춤·인자 오류 1.
- git 2.31.0 이상이 필요하다. `\wsl.localhost\` · `\wsl$\` 경로는 그 배포판의 git, 나머지는 Git for Windows 를 쓴다.
- 데이터: `%APPDATA%\GitBridge2`(settings.json, history.jsonl), `%LOCALAPPDATA%\GitBridge2`(logs, temp). `GITBRIDGE2_HOME` 이 있으면 그 아래 roaming·local 을 쓴다.
