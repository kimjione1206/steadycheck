# 실제 PC 에서 steadycheck 시험하기

[English](TESTING.md)

## 1. 부탁드리는 것

실제 PC 가 있어야 답할 수 있는 두 가지입니다.
- 실제 불안정을 **잡는지** — 불안정한 오버클럭, 너무 빡빡한 메모리 타이밍, 너무 낮은 전압, 불량 부품.
- 안정한 PC 에서는 **조용한지**(PASS) — 헛경보가 없는지.

지금까지 확인한 것(모두 깃허브 서버에서):
- **흉내 낸 고장:** 메모리 기본 세트를 고장 하나를 넣은 흉내 메모리에서, 정해진 고장 목록 전부에 대해 돌립니다([How it works](HOW-IT-WORKS.md) 의 표).
- **바깥에서 넣은 고장:** 고치지 않은 프로그램이 도는 동안 그 메모리의 비트 하나를 뒤집거나 고정하고, CPU 계산 결과의 비트 하나를 뒤집습니다(`verify/`).
- **변이 시험:** 코드를 일부러 조금씩 망가뜨려 시험이 알아채는지 봅니다.

아직 안 한 것: 실제 불량·불안정 하드웨어 — 여러분의 결과가 채워 줄 부분입니다. 알려진 한계: DDR5 는 칩 안에서 비트 하나짜리 오류를 알리지 않고 고치고, 로해머(RowHammer)는 시험하지 않으며, 온도·타이밍에 따라 나타나는 고장은 오래 돌려야 드러날 기회가 생깁니다.

## 2. 안전

- steadycheck 는 CPU 와 메모리에 높은 부하를 겁니다. PC 가 뜨거워지고 팬 소리가 커집니다.
- 노트북·냉각이 약한 PC: 온도를 지켜보고 너무 높으면 Ctrl+C 로 멈추세요.
- 하던 작업은 먼저 저장하고 닫으세요. 불안정한 설정에서는 PC 가 멈추거나 다시 켜질 수 있습니다.
- 중요한 파일은 백업하고, 가능하면 시험용으로 따로 설치한 윈도우에서 돌리세요. 불안정한 메모리는 디스크의 파일과 윈도우 자체를 망가뜨릴 수 있습니다.
- 평소 안전하다고 보는 범위를 넘어 전압을 올리지 마세요. 더 빡빡한 타이밍, 같은 전압에서 더 높은 속도, 더 낮은 전압만으로도 불안정한 설정은 충분히 만들 수 있습니다.
- 실행은 본인 책임입니다. steadycheck 는 어떤 보증도 없이 "있는 그대로(AS IS)" 제공됩니다(MIT 라이선스, [LICENSE](../LICENSE)).

## 3. 받기와 확인

받은 파일은 한 폴더에 둡니다. 그 폴더에서 PowerShell 을 열려면 파일 탐색기에서 폴더의 빈 곳을 마우스 오른쪽 단추로 누르고 "터미널에서 열기"를 고릅니다(윈도우 10 에서는 Shift 키를 누른 채 오른쪽 단추를 누르고 "여기에 PowerShell 창 열기"를 고릅니다). 이 문서의 명령은 그 창에 입력합니다.

명령에 있는 `<version>` 은 받은 파일 이름에 있는 숫자로 바꿔 입력합니다. 예를 들어 `steadycheck-0.6.0-windows-x86_64.exe` 를 받았다면 `.\steadycheck-0.6.0-windows-x86_64.exe all --mb auto --seconds 300 > result.json` 처럼 입력합니다(3단계에서는 `refs/tags/v0.6.0`).

1. [Releases](https://github.com/kimjione1206/steadycheck/releases) 에서 `steadycheck-<version>-windows-x86_64.exe` 와 `SHA256SUMS.txt` 를 받습니다.
2. 지문 확인: `certutil -hashfile steadycheck-<version>-windows-x86_64.exe SHA256` 이 `SHA256SUMS.txt` 와 같은 값을 찍어야 합니다.
3. 선택 — 어디서 만들어졌는지 확인(GitHub CLI, 먼저 `gh auth login` 으로 한 번 로그인):
   `gh attestation verify steadycheck-<version>-windows-x86_64.exe -R kimjione1206/steadycheck --source-ref refs/tags/v<version> --signer-workflow kimjione1206/steadycheck/.github/workflows/release.yml`
4. 윈도우 SmartScreen 이 "게시자를 알 수 없음" 경고를 띄울 수 있습니다. 코드 서명 인증서로 서명하지 않았기 때문이며, 위 확인이 진짜 빌드인지 확인하는 방법입니다. 윈도우 11 에서 스마트 앱 컨트롤(Smart App Control)이 켜져 있으면 서명 없는 exe 는 "실행" 선택지 없이 아예 막히므로, 그 PC 에서는 스마트 앱 컨트롤이 꺼져 있어야 steadycheck 를 돌릴 수 있습니다. 끌지 말지는 본인이 정할 일입니다.
5. [`tools/collect-info.ps1`](../tools/collect-info.ps1) 을 받아("Download raw file" 단추) exe 와 같은 폴더에 둡니다.

## 4. 실행

그 폴더에서 PowerShell 을 엽니다.
1. 다른 프로그램(브라우저, 게임, 런처)을 끕니다. `share` 시험은 다른 일이 없는 PC 가 필요합니다.
2. 하드웨어 정보를 모으고 찍힌 글을 복사합니다:
   `powershell -ExecutionPolicy Bypass -File .\collect-info.ps1`
   윈도우·CPU·메인보드·BIOS·메모리 모듈 정보만 찍습니다 — 일련번호, 사용자·컴퓨터 이름, 네트워크 정보, 제품 키는 넣지 않습니다. steadycheck 버전을 읽으려고 exe 를 1초 돌립니다.
3. 시험을 돌리고 JSON 을 저장합니다:
   `.\steadycheck-<version>-windows-x86_64.exe all --mb auto --seconds 300 > result.json`
   `all` 은 cpu → share → mem 을 각각 300초씩, 합쳐 약 15분 돌고, 첫 실패에서 멈춥니다. 메모리에 집중하려면 대신 `mem --mb auto --seconds 600` 을 씁니다. 불안정이 의심되는 설정에서는 `--keep-going 20` 을 붙이세요(`mem --mb auto --seconds 600 --keep-going 20`): 메모리 검사가 첫 오류에서 멈추지 않고 오류 20개까지 계속 돕니다 — 오류가 몇 개·얼마 간격으로 나는지가 설정 한계를 보는 데 도움이 됩니다.
4. 결과 확인: 실행이 끝난 직후 `$LASTEXITCODE` 를 입력하고 Enter 를 누릅니다. 0 이면 PASS, 1 이면 FAIL, 2 는 지원하지 않는 환경, 3 은 사용법 오류, 4 = 판단 보류 — 오류는 없지만 검사가 덜 됨, `--seconds` 를 늘려 다시 돌리세요. 출고 검사처럼 시간이 짧아 합격으로 착각하면 안 될 때는 `--require-complete` 를 붙이세요: 메모리 기본 세트를 못 끝냈거나 `mem.rounds_d` 가 4 미만이면 오류가 없어도 0 대신 4(INCOMPLETE)로 끝납니다. 시험이 끝나면 창의 마지막 줄에 `판정: PASS` 또는 `판정: FAIL`(`--require-complete` 를 줬을 때는 `판정: INCOMPLETE` 도) 이 나옵니다. 이 줄은 표준 오류(stderr)로 나가서 `> result.json` 으로 파일에 들어가지 않고 창에 남습니다. 전체 결과는 `result.json` 에 있습니다.

## 5. 결과 읽기

- `verdict`: `PASS`, `FAIL`, 또는 `INCOMPLETE`(`--require-complete` 를 줬을 때만).
- `warnings`: `mem_base_incomplete` 는 메모리 기본 세트를 시간 안에 못 끝냈다는 뜻이라, 메모리 고장 표가 이 실행에는 해당하지 않습니다. `--seconds` 를 늘려 다시 돌리세요(`mem.base_seconds_estimate` 가 기본 세트에 걸리는 시간). 판정은 바뀌지 않습니다.
- `mem.rounds_d`: 4 이상이어야 합니다. 대략 `mem.base_seconds_estimate` 의 3~4배 이상 돌리세요. 결과의 `rounds_d` 가 4 미만이면 `--seconds` 를 늘려 다시 돌리세요.
- `share.min_thread_messages`: 0 이면 한 통도 못 받은 일꾼이 있다는 뜻입니다. 바쁜 PC 에서는 다른 프로그램이 CPU 를 차지해서일 수 있으니 끄고 다시 돌리세요.
- FAIL 이면 실패한 부분에 `error` 가 있습니다. `mem.error.kind` 는 참고 단서일 뿐입니다: `read`(메모리 안 값은 맞고 읽어 오는 과정이 틀림), `stored`(틀린 값이 메모리에 남아 있음). `panic` 은 하드웨어 고장이 아니라 steadycheck 버그이니 빈 이슈로 알려 주세요.
- `mem.errors_total`: 잡은 메모리 오류 수. `--keep-going` 을 2 이상으로 주면 `mem.errors` 에 오류 목록(앞 32개)이 있고(`--keep-going 1` 은 기본 실행과 같음), `mem.errors[].at_ms` 는 각 오류를 잡은 시각(시작부터 밀리초)이라 오류가 얼마나 자주 나는지 볼 수 있습니다.
- `mem.tested_percent`: 컴퓨터 전체 실제 메모리(`mem.total_phys_bytes`) 중 검사한 비율(%). 윈도우와 다른 프로그램이 쓰는 몫은 윈도우 안에서 검사할 수 없어 보통 100 보다 작습니다.
- `whea`(참고, 판정은 바뀌지 않음): 윈도우 하드웨어 오류 기록(WHEA-Logger)이 검사 중(`whea.during_run`, 사건 번호별 수)과 검사 전 7일(`whea.before_7_days`)에 몇 건 남았는지. `during_run` 에 숫자가 있으면 이벤트 뷰어 → Windows 로그 → 시스템에서 원본 WHEA-Logger 항목을 확인하세요.

## 6. 결과 보내기

[새 이슈](https://github.com/kimjione1206/steadycheck/issues/new/choose)를 열고 고릅니다:
- **Tested on a known-unstable setting** — 불안정한 줄 아는 설정에서 돌렸을 때. PASS 든 FAIL 이든 도움이 됩니다.
- **FAIL on a PC I believe is stable** — 안정하다고 볼 근거가 있는 PC 에서 FAIL 이 났을 때.

`collect-info.ps1` 이 찍은 표, 실행한 명령, `result.json` 내용을 붙여 넣으세요. 결과는 메모장으로 열어(`notepad result.json`) 전부 복사하면 됩니다. 윈도우 기본 PowerShell 5.1 은 이 파일을 UTF-16 으로 저장하지만 그대로 괜찮으니 인코딩을 바꿀 필요는 없습니다. 창에 나온 빨간 오류 글은 붙이지 마세요. 사용자 이름이 들어간 폴더 경로가 나올 수 있습니다. 일련번호·이름·IP 주소·제품 키는 붙이지 마세요. 결과 JSON 에는 개인정보가 없습니다.
