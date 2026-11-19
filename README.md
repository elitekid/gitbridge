# gitbridge-rs

GitBridge(서버에 바로 닿지 않는 PC 와 git 번들로 주고받는 도구)의 Rust판이다. 파이썬판 1.0.5를 대체하며 첫 정식판은 2.0.0이다.

- 대상: Windows(Git for Windows, WSL 경로 저장소 포함)
- 빌드: 인터넷이 되는 PC에서 한다. 다른 PC에는 빌드한 exe만 옮긴다.
- 작업 방식: main 기준 작업트리의 feature 브랜치에서 작업한다.
