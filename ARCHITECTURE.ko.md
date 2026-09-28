# AionCore 아키텍처

[English](./ARCHITECTURE.md) · [简体中文](./ARCHITECTURE.zh-CN.md)

AionCore는 AionUi의 백엔드 서버다. Rust로 작성되었으며 Axum, Tokio, SQLite를 사용한다. 클라이언트에 HTTP REST API와 WebSocket 실시간 이벤트를 제공한다.

이 문서는 [영문 아키텍처 문서](./ARCHITECTURE.md)의 한국어판이다. 설명 범위는 이 저장소의 AionCore이며, AI Builder 플랫폼 전체의 인증·클라우드 격리·배포 구조는 별도 범위다. 패키지 목록은 현재 [Cargo.toml](./Cargo.toml)에 맞춰 27개로 보완했다.

## 기술스택

| 구성 요소 | 기술 | 역할 |
|-----------|------|------|
| 웹 프레임워크 | Axum 0.8 | HTTP 라우팅, 요청 처리, 응답 생성 |
| 비동기 런타임 | Tokio | 비동기 작업과 네트워크 입출력 실행 |
| 데이터베이스 | SQLite + sqlx | 데이터 저장과 비동기 DB 접근 |
| 인증·요청 보호 | JWT + CSRF Double Submit Cookie | 사용자 인증과 교차 사이트 요청 위조 방어 |
| 실시간 통신 | WebSocket + 이벤트 브로드캐스트 | 메시지·작업 상태 변경을 클라이언트에 전달 |

## 전체 구조

```text
aionui-app                         실행 진입점, 서비스·라우터 조립
  │
  ├─ conversation / channel / team / ...  도메인별 기능
  │
  ├─ aionui-auth / aionui-realtime        인증, WebSocket, 이벤트
  │
  └─ aionui-db / aionui-api-types / aionui-runtime / ...
     aionui-common / aionui-assets       DB, API 타입, 실행 환경, 공통 기능
```

계층 간 의존성은 위에서 아래로 향한다. 도메인 패키지는 `aionui-app`에 의존하지 않으며, `aionui-common`은 다른 내부 패키지에 의존하지 않는다.

## 패키지 계층

프로젝트는 27개 crate를 포함하는 Cargo workspace로 구성된다. **crate는 Rust의 패키지·컴파일 단위**이며, 여기서는 `crates/` 아래 각 디렉터리가 하나의 패키지다. workspace는 이 패키지들의 빌드와 의존성을 함께 관리한다.

### Foundation — 기반 계층

다른 패키지들이 공통으로 사용하는 기능이다. 변경 시 영향을 받는 범위를 확인해야 한다.

| 패키지 | 담당 역할 |
|--------|-----------|
| `aionui-common` | 공통 오류 타입(`ApiError`), 열거형, ID 생성, 암호화 유틸리티, 타임스탬프, 페이지네이션 |
| `aionui-api-types` | HTTP·WebSocket 요청과 응답 타입. API 계약을 정의하는 공통 기준 |
| `aionui-db` | SQLite 데이터 접근 계층. Repository trait과 구현체 정의 |
| `aionui-assets` | 실행 파일에 포함되는 정적 데이터. 에이전트 메타데이터, 프롬프트 등 |
| `aionui-runtime` | 관리형 Node 실행 환경, 하위 프로세스 실행, PATH 보완 |
| `aionui-process` | 직접 실행하는 CLI 세션의 하위 프로세스 수명 관리, 실행 범위 관리, 시작 시 잔여 프로세스 정리 |

### Capability — 공통 기능 계층

여러 도메인이 함께 사용하는 인증·실시간 통신 기능이다.

| 패키지 | 담당 역할 |
|--------|-----------|
| `aionui-auth` | JWT 인증, 비밀번호 해싱, CSRF 보호, 쿠키 관리, 인증 미들웨어 |
| `aionui-realtime` | WebSocket 연결 관리, `BroadcastEventBus`를 통한 이벤트 전달, 메시지 라우팅 |

### Domain — 업무 기능 계층

각 패키지가 하나의 기능 영역을 담당하며, 영역 간 결합을 최소화한다.

| 패키지 | 담당 역할 |
|--------|-----------|
| `aionui-conversation` | 대화 관리, 메시지 전송, 사용자 확인 요청, 응답 스트리밍 |
| `aionui-session` | 직접 실행하는 CLI와 ACP 백엔드의 세션 상태·기능·명령·이벤트를 공통 인터페이스로 관리 |
| `aionui-session-message` | 세션 간 메시지 전달. 사용자가 메시지를 보내는 기존 `ConversationService::send_message` 경로를 사용 |
| `aionui-skill-runtime` | 실행 중인 대화에 허용된 Skill 목록·내용의 읽기 전용 조회. Skill 설치·설정 관리와 권한 분리 |
| `aionui-channel` | WeChat·DingTalk·Lark 등 외부 채널 연동, 플러그인, 페어링 세션 |
| `aionui-team` | 에이전트 팀의 협업, 작업 스케줄링, 메일박스 |
| `aionui-team-prompts` | 에이전트 팀의 역할 프롬프트, 운영 규칙, 도구 사용 권한 메타데이터 |
| `aionui-cron` | 예약 작업 실행, cron 표현식 처리, 이벤트 발생 |
| `aionui-file` | 파일 작업, 변경 감시, 스냅샷, Git 작업, 압축 |
| `aionui-project` | 프로젝트와 폴더 연결, Project Explorer, 파일 시스템 감시, 리소스 접근 범위 관리 |
| `aionui-sidebar` | 사이드바 표시 데이터 구성. 고정 항목·프로젝트·대화 분류, 조회 범위, 정렬 관리 |
| `aionui-office` | Excel·PowerPoint·Word 문서 처리, 미리보기, 변환 |
| `aionui-system` | 시스템 설정, 모델 제공자 관리, 버전 확인, 모델 목록 조회 |
| `aionui-mcp` | MCP 연동, OAuth, 플랫폼별 어댑터 |
| `aionui-ai-agent` | 에이전트 수명 관리, 작업 큐, ACP 연동, 보조 Skill 처리 |
| `aionui-extension` | Extension 등록·허브 관리, Skill 검색·설치 |
| `aionui-shell` | 셸 명령 실행, 음성의 텍스트 변환 |
| `aionui-assistant` | Assistant 구성과 관리 |

영문 목록에 없던 세 패키지는 각 진입점의 설명을 확인해 추가했다: [session-message](./crates/aionui-session-message/src/lib.rs), [skill-runtime](./crates/aionui-skill-runtime/src/lib.rs), [sidebar](./crates/aionui-sidebar/src/lib.rs).

### Composition — 조립 계층

| 패키지 | 담당 역할 |
|--------|-----------|
| `aionui-app` | 실행 파일의 진입점. 각 패키지의 서비스와 라우터를 조립해 Axum 서버 구성 |

### 의존성 방향 규칙

```text
Composition → Domain → Capability → Foundation
              Domain → Foundation  (중간 계층을 건너뛰는 의존도 허용)
```

- 상위 계층은 하위 계층에 의존할 수 있다.
- 같은 계층의 기능은 trait으로 추상화된 인터페이스를 통해 사용한다. 예를 들어 conversation은 `IWorkerTaskManager`로 ai-agent 기능을 사용한다.
- 하위 계층에서 상위 계층으로의 의존은 금지한다.
- 순환 의존은 금지한다.

## 도메인 패키지 내부 구조

도메인 패키지는 일관된 내부 구조를 따른다. `aionui-conversation`을 기준으로 설명한다.

### 표준 디렉터리 구조

```text
crates/aionui-conversation/src/
├── lib.rs       # 모듈 선언과 공개 API 내보내기
├── routes.rs    # HTTP 라우트와 요청 처리 함수
├── service.rs   # 비즈니스 로직
├── state.rs     # 서비스·의존성을 보관하는 RouterState
├── error.rs     # 도메인 전용 오류 타입 (선택)
├── types.rs     # 도메인 모델 (선택)
└── [modules]    # streaming.rs 등 기능별 하위 모듈
```

### 파일별 책임

**`lib.rs` — 패키지 진입점과 공개 API**

- 모듈을 선언하고 외부에 공개할 항목을 내보낸다.
- `domain_routes()`, `Service`, `RouterState` 등을 공개한다.
- 비즈니스 로직은 넣지 않는다.

**`routes.rs` — HTTP 요청과 응답 처리**

- `domain_routes(state: RouterState) -> Router` 함수를 공개한다.
- 각 핸들러는 매개변수 추출 → 서비스 호출 → 응답 생성 순서로 처리한다.
- HTTP 변환을 담당하며 비즈니스 판단은 서비스에 맡긴다.

**`service.rs` — 비즈니스 로직**

- 생성자로 Repository trait 객체, EventBroadcaster 등 필요한 의존성을 전달받는다.
- 업무 규칙, 규칙 검증, 여러 작업을 조합하는 로직을 담당한다.
- axum을 가져오거나 HTTP 타입을 직접 다루지 않는다.

**`state.rs` — 라우터에 전달할 의존성**

- 서비스 인스턴스와 다른 의존성의 `Arc` 참조를 보관한다.
- Axum이 요구하는 `Clone`을 구현한다.

### 핸들러 함수 형식

```rust
async fn handler(
    State(state): State<RouterState>,         // Dependency injection
    Extension(user): Extension<CurrentUser>, // Authenticated user
    Path(id): Path<String>,                  // Path parameter
    Json(body): Json<RequestType>,           // Request body
) -> Result<(StatusCode, Json<ApiResponse<ResponseType>>), ApiError>
```

`State`는 서비스 의존성, `Extension`은 인증된 사용자, `Path`는 URL 매개변수, `Json`은 요청 본문을 가져온다.

### 새 패키지 생성과 기존 패키지 확장 기준

**새 패키지를 만드는 경우:**

- 자체 데이터 모델과 수명 주기를 가진 독립적인 기능 영역이다.
- `/api/new-domain/...`처럼 독립적인 라우트 접두사가 필요하다.
- 기존 영역과 강하게 결합되어 있지 않다.

**기존 패키지를 확장하는 경우:**

- 기존 기능 영역의 하위 기능이다.
- 같은 데이터 모델을 공유한다.
- 기존 라우트 접두사 아래에 경로를 추가하면 된다.

## API 규칙

### REST 경로 이름

```text
/api/{resources}                     # 목록 조회(GET), 생성(POST)
/api/{resources}/{id}                # 상세 조회(GET), 수정(PATCH), 삭제(DELETE)
/api/{resources}/{id}/{subresources} # 하위 리소스
/api/{resources}/{id}/{action}       # CRUD로 표현하기 어려운 동작
```

- 모든 경로에 `/api/` 접두사를 사용한다.
- 리소스 이름과 경로 구간은 `ai-agents`, `qr-login`처럼 kebab-case로 작성한다.
- 동작 경로에는 `reset`, `stop`, `run`처럼 동사나 동사구를 사용한다.

### 공통 응답 형식

성공 응답은 `ApiResponse<T>`를 사용한다. 아래의 `{ ... }`는 실제 데이터가 들어갈 자리를 나타낸다.

```json
{
  "success": true,
  "data": { ... },
  "message": "optional message"
}
```

`data`와 `message`는 선택 필드이며, 값이 없으면 직렬화된 응답에서 생략한다.

오류 응답은 `ErrorResponse`를 사용한다.

```json
{
  "success": false,
  "error": "Human-readable error message",
  "code": "ERROR_CODE"
}
```

공통 요청·응답 타입은 `aionui-api-types`에 정의한다.

### HTTP 상태 코드 매핑

| ApiError 종류 | HTTP 상태 | 오류 코드 | 사용 상황 |
|---------------|-----------|-----------|-----------|
| BadRequest | 400 | BAD_REQUEST | 잘못된 요청 매개변수 |
| Unauthorized | 401 | UNAUTHORIZED | 미인증 또는 토큰 만료 |
| Forbidden | 403 | FORBIDDEN | 접근 권한 없음 |
| NotFound | 404 | NOT_FOUND | 리소스 없음 |
| Conflict | 409 | CONFLICT | 리소스 충돌 |
| UnprocessableEntity | 422 | UNPROCESSABLE_ENTITY | 요청의 의미·내용이 유효하지 않음 |
| RateLimited | 429 | RATE_LIMITED | 요청 빈도 제한 초과 |
| Internal | 500 | INTERNAL_ERROR | 서버 내부 오류 |
| BadGateway | 502 | BAD_GATEWAY | 상위 서비스 오류 |
| Timeout | 502 | TIMEOUT | 상위 서비스 응답 시간 초과 |

### 페이지네이션

offset 기반 페이지네이션인 `PaginatedResult<T>`를 사용한다.

```json
{
  "items": [...],
  "total": 100,
  "hasMore": true
}
```

- `items`: 현재 페이지의 데이터.
- `total`: 전체 항목 수.
- `hasMore`: 다음 데이터의 존재 여부.

JSON 필드 이름은 `#[serde(rename_all = "camelCase")]`를 통해 camelCase로 직렬화한다.

### WebSocket 이벤트 규칙

연결 진입점은 `/ws` 하나다. 메시지는 `WebSocketMessage<T>` 형식을 사용한다.

```json
{
  "name": "domain.actionName",
  "data": { ... }
}
```

- 이벤트 이름은 `{domain}.{actionName}`의 두 단계로 구성한다.
- domain은 `conversation`, `fileWatch`처럼 camelCase를 사용한다.
- actionName도 `listChanged`, `statusChanged`처럼 camelCase를 사용한다.
- 예: `conversation.listChanged`, `cron.jobExecuted`, `extensions.stateChanged`.

기존 이벤트 중에는 `channel.pairing-requested`처럼 kebab-case를 사용하거나 `team.agent.status`처럼 세 단계로 구성된 이름이 있다. 새 이벤트는 위 규칙을 따르고, 기존 이름은 관련 모듈을 수정할 때 점진적으로 정리한다.

### ACP 도구 출력 정리

ACP 에이전트의 도구 호출 이벤트는 `aionui-ai-agent`의 변환 계층에서 공통 `AgentStreamEvent`로 바뀐다. 이때 큰 이미지 데이터를 그대로 WebSocket으로 전달하거나 SQLite에 저장하지 않도록 정리한다.

[출력 변환 코드](./crates/aionui-ai-agent/src/protocol/events/translate.rs)는 `raw_output.result`에서 64 KiB를 초과하는 인라인 이미지로 판별된 문자열을 제거하고, `result_omitted`, `result_omitted_reason`, `result_bytes`를 기록한다. `saved_path`가 있으면 `image.path` 등 파일 참조를 구성한다. 저장 경로가 없어도 큰 인라인 이미지 제거는 적용한다.

클라이언트는 파일 경로로 이미지를 요청하는 방식을 사용하며, 응답에 전체 base64 데이터가 포함된다고 가정하지 않는다.

## 데이터 계층

### Repository trait 패턴

DB 접근은 `aionui-db`에 정의된 trait을 통해 수행한다. **trait은 구현체가 제공해야 하는 동작을 정의하는 인터페이스**다.

```rust
#[async_trait]
pub trait IConversationRepository: Send + Sync {
    async fn get(&self, id: &str) -> Result<Option<ConversationRow>, DbError>;
    async fn create(&self, row: &ConversationRow) -> Result<(), DbError>;
    async fn update(&self, id: &str, params: &UpdateConversationParams) -> Result<(), DbError>;
    async fn delete(&self, id: &str) -> Result<(), DbError>;
    // ...
}
```

- 각 도메인 엔티티에 `IConversationRepository`, `IUserRepository` 같은 Repository trait을 둔다.
- trait 이름에는 인터페이스를 뜻하는 `I` 접두사를 붙인다.
- 구현체에는 `SqliteConversationRepository`처럼 `Sqlite` 접두사를 붙인다.
- 서비스는 구체적인 SQLite 구현체가 아닌 trait에 의존한다.

### 데이터 타입의 위치

| 타입 | 위치 | 용도 | 예시 |
|------|------|------|------|
| Row 모델 | `aionui-db/src/models/` | DB 행과 Rust 구조체 매핑 | `ConversationRow` |
| Params 객체 | `aionui-db/src/repository/` | DB 쓰기 매개변수 | `UpdateConversationParams` |
| 요청·응답 타입 | `aionui-api-types` | API 계약과 공통 데이터 전달 객체(DTO) | `CreateConversationRequest`, `ConversationResponse` |

서비스 계층은 `aionui-api-types`의 타입을 직접 사용할 수 있다. 이 패키지는 HTTP 프레임워크에 의존하지 않는 순수 데이터 구조를 정의한다.

따라서 `aionui-api-types`는 axum, tower 등 HTTP 프레임워크에 의존하면 안 된다. serde와 기본 타입 관련 의존성만 허용한다.

### 계층별 책임

- **Handler (`routes.rs`)**: 요청 검증, 매개변수 추출, 오류 매핑, `ApiResponse` 생성.
- **Service (`service.rs`)**: 비즈니스 로직, 업무 규칙 검증, Repository 호출 조합, Row와 응답 타입 간 변환.
- **Repository (`aionui-db`)**: 비즈니스 판단이 없는 DB 작업.

Handler와 Service의 경계는 사용하는 타입보다 책임으로 구분한다. Handler는 업무 판단을 하지 않고, Service는 HTTP 처리를 하지 않는다.

### 마이그레이션 관리

sqlx의 내장 마이그레이션 기능인 `sqlx::migrate!()`를 사용한다.

- 파일 위치: `crates/aionui-db/migrations/`.
- 파일 이름: `NNN_descriptive_name.sql`. 순차 번호를 사용한다.
- 애플리케이션 시작 시 마이그레이션을 실행한다.
- 테이블 추가나 스키마 변경은 반드시 마이그레이션 파일로 관리한다. DB를 수동으로 수정하지 않는다.
- `IF NOT EXISTS`를 사용해 같은 작업이 반복되어도 중복 생성되지 않도록 한다.

### 오류 전달

영문 문서의 기본 오류 변환 흐름은 다음과 같다.

```text
DbError (DB 계층)
  ↓ From 구현으로 변환 (aionui-db/src/error.rs)
ApiError (HTTP 경계의 공통 오류)
  ↓ IntoResponse 구현으로 변환
HTTP 응답 (상태 코드 + ErrorResponse JSON)
```

- `DbError::NotFound` → `ApiError::NotFound`: 리소스 없음이라는 의미 유지.
- `DbError::Conflict` → `ApiError::Conflict`: 충돌이라는 의미 유지.
- `DbError::Query`, `Migration`, `Init` → `ApiError::Internal`: 내부 상세 정보 숨김.

현재 [기여 규칙](./AGENTS.md)에 따라 `ApiError`는 라우트·미들웨어 등 HTTP 경계에서 사용한다. 서비스·도메인 내부에서는 `ConversationError`, `TeamError` 같은 해당 패키지의 오류 타입을 우선 사용하고, 라우트에서 `ApiError`로 변환한다.

## 의존성 주입

### 주입 흐름

Axum의 `with_state()`를 사용해 서비스와 의존성을 요청 처리 코드에 전달한다.

**1. `AppServices`에서 공통 의존성 준비**

`aionui-app`의 `AppServices`는 DB, 인증 서비스, Repository, 이벤트 버스 등 공통 의존성을 보관한다. 영문 문서의 구조 예시는 다음과 같다.

```rust
pub struct AppServices {
    pub database: Database,
    pub jwt_service: Arc<JwtService>,
    pub user_repo: Arc<dyn IUserRepository>,
    pub cookie_config: Arc<CookieConfig>,
    pub qr_token_store: Arc<QrTokenStore>,
    pub ws_manager: Arc<WebSocketManager>,
    pub event_bus: Arc<BroadcastEventBus>,
    pub worker_task_manager: Arc<dyn IWorkerTaskManager>,
    pub agent_registry: Arc<AgentRegistry>,
    pub conversation_repo: Arc<dyn IConversationRepository>,
    pub acp_session_sync: Arc<AcpSessionSyncService>,
    pub encryption_secret_raw: String, // Storage-encryption root, separate from JWT signing
    pub data_dir: String,
    pub local: bool,
    pub app_version: String,
    pub skill_paths: Arc<SkillPaths>,
    pub guide_mcp_config: Option<GuideMcpConfig>,
    // ...
}
```

`Arc`는 여러 작업이 같은 객체를 안전하게 공유하도록 참조 수명을 관리하는 타입이다. `Arc<dyn Trait>`은 구체적인 구현체를 고정하지 않고 trait을 통해 공유한다.

**2. 도메인별 `RouterState` 구성**

`build_module_states()`는 `AppServices`에서 각 도메인이 필요한 의존성만 꺼내 해당 `RouterState`를 구성한다.

```rust
// Simple domain: one service
pub struct CronRouterState {
    pub cron_service: Arc<CronService>,
}

// Complex domain: multiple services
pub struct OfficeRouterState {
    pub watch_manager: Arc<OfficecliWatchManager>,
    pub snapshot_service: Arc<SnapshotService>,
    pub conversion_service: Arc<ConversionService>,
    pub proxy_service: Arc<ProxyService>,
}
```

RouterState는 `#[derive(Clone)]`을 적용하고 의존성을 `Arc`로 감싸 보관한다.

**3. 핸들러에서 `State`로 의존성 사용**

```rust
async fn create(
    State(state): State<ConversationRouterState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<CreateConversationRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ApiResponse<ConversationResponse>>), ApiError> {
    let Json(req) = body.map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let conversation = state.conversation_service.create(&user.id, req).await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::ok(conversation))))
}
```

### 라우터 조립

세 단계의 함수가 라우터를 구성한다.

1. `create_router()`: 비동기 진입점. 상태를 구성한 뒤 다음 단계 호출.
2. `create_router_with_states()`: 외부에서 구성한 `ModuleStates`를 전달할 수 있는 단계. 테스트 등에 사용.
3. `create_router_with_all_state()`: 라우트와 미들웨어를 최종 결합.

요청은 바깥쪽부터 다음 순서로 처리된다.

```text
CORS (로컬 모드)
  → 보안 헤더 (모든 요청)
    → CSRF (로컬이 아닌 모드)
      → 인증 미들웨어 (라우트 그룹별 적용)
        → Handler
```

- 인증 미들웨어는 전역 적용이 아니라 `route_layer()`로 필요한 라우트 그룹에 적용한다.
- 로그인·상태 확인 등 공개 라우트에는 인증 미들웨어를 붙이지 않는다.
- `/ws`는 HTTP 인증 미들웨어 대신 별도의 토큰 검증 콜백을 사용한다.
- 로컬 모드에서는 CSRF 검사를 생략하고 기본 시스템 사용자를 주입한다.

### 의존성 구성 규칙

- `AppServices`와 `aionui-app`의 조립 함수에서 Repository 생성과 서비스 구성을 관리한다.
- 각 `RouterState`에는 해당 도메인이 실제 사용하는 의존성만 넣는다.
- 교체 가능한 의존성은 `Arc<dyn Trait>`으로 전달해 구현 교체와 테스트 대체를 지원한다.
- 도메인 패키지는 필요한 의존성을 정의하고, 실제 조립은 `aionui-app`에 맡긴다.

## 보안 모델

이 절은 영문 문서에 정의된 **AionCore 자체의 보안 모델**을 설명한다. AI Builder 플랫폼의 외부 인증 설정과 구분한다.

### 미들웨어 순서

```text
CORS (로컬 모드에서만 모든 origin 허용)
  → 보안 헤더
      X-Frame-Options: DENY
      X-Content-Type-Options: nosniff
      X-XSS-Protection: 1; mode=block
      Referrer-Policy: strict-origin-when-cross-origin
    → CSRF (로컬이 아닌 모드, Double Submit Cookie)
      → 인증 미들웨어 (라우트 그룹별 적용)
        → Handler
```

### JWT 인증

- 서명 알고리즘: HMAC-SHA256.
- 토큰 유효기간: 24시간.
- 토큰 내용: `user_id`, `username`, `iat`, `exp`, `iss` (`aionui`), `aud` (`aionui-webui`).
- 서명 비밀키 선택 순서: 환경변수 → DB → `getrandom`으로 64바이트 무작위 생성.
- 요청에서 토큰을 찾는 순서: `Authorization: Bearer` 헤더 → `aionui-session` 쿠키.
- 토큰 차단 목록은 SHA-256 해시와 DashMap을 사용한다.

### CSRF 보호

쿠키와 요청 헤더에 같은 값을 보내는 Double Submit Cookie 패턴을 사용한다.

- 쿠키 이름: `aionui-csrf-token`. JavaScript에서 읽어야 하므로 HttpOnly를 사용하지 않는다.
- 요청 헤더: `x-csrf-token`.
- 검증 조건: 쿠키 값과 헤더 값이 정확히 일치해야 한다.
- GET·HEAD·OPTIONS 요청은 검증에서 제외한다.
- 예외 경로: `/login`, `/api/auth/qr-login`.

### 비밀번호 보호

- bcrypt를 사용하며 비용 계수는 12다.
- 응답 시간을 최소 50ms로 유지해 시간 차이를 이용한 공격을 완화한다.
- 존재하지 않는 사용자도 미리 계산된 더미 해시로 처리해 계정 존재 여부 추측을 어렵게 한다.

### 쿠키 설정

| 쿠키 | HttpOnly | Secure | SameSite | Max-Age |
|------|----------|--------|----------|---------|
| `aionui-session` | 적용 | HTTPS일 때 적용 | HTTPS: Strict / HTTP: Lax | 30일 |
| `aionui-csrf-token` | 미적용 | HTTPS일 때 적용 | HTTPS: Strict / HTTP: Lax | 30일 |

### 요청 빈도 제한

| 구분 | 제한 | 시간 범위 | 적용 대상 | 구분 기준 |
|------|------|-----------|-----------|-----------|
| Auth | 인증 실패 5회 | 15분 | 로그인 라우트 | 클라이언트 IP |
| API | 요청 60회 | 1분 | 공개 엔드포인트 | 클라이언트 IP |
| Action | 요청 20회 | 1분 | 민감한 작업 | 사용자 ID, 없으면 IP |

IP 추출 순서는 `X-Forwarded-For` → `X-Real-IP` → `unknown`이다.

### 로컬 모드

Electron에 내장해 실행하는 상황을 위한 모드이며, `--local` 옵션으로 활성화한다.

- JWT 검증을 생략하고 `system_default_user`를 사용한다.
- CSRF 검사를 생략한다.
- 모든 origin을 허용하는 CORS를 활성화한다.
- WebSocket 인증도 생략한다.

### 보안 규칙

- 엔드포인트를 추가할 때 인증 미들웨어 적용 여부를 검토한다.
- 상태를 변경하는 POST·PUT·DELETE·PATCH 요청에 CSRF 보호를 적용한다.
- 민감한 작업에 요청 빈도 제한을 설정한다.
- 오류 응답에 내부 구현 정보를 노출하지 않는다. 예를 들어 `DbError::Query`는 일반적인 `Internal` 오류로 변환한다.
- 비밀값을 소스 코드에 하드코딩하지 않는다.

## 테스트 전략

### 테스트 계층

| 계층 | 위치 | DB 구성 | 목적 |
|------|------|---------|------|
| 단위 테스트 | 각 `.rs` 파일의 `#[cfg(test)]` 모듈 | DB 없음 또는 Mock | 함수 단위 로직 검증 |
| 통합 테스트 | `crates/<crate>/tests/` | 메모리 SQLite | Service·Repository 동작 검증 |
| E2E 테스트 | `crates/aionui-app/tests/` | 메모리 SQLite | HTTP 요청 처리 전체 흐름 검증 |

### 메모리 DB

DB가 필요한 테스트는 `init_database_memory()`를 사용한다.

- `sqlite::memory:`로 메모리 DB를 생성한다.
- 메모리 DB의 데이터 일관성을 위해 연결 수를 하나로 제한한다(`max_connections = 1`).
- 마이그레이션을 자동 실행한다.
- 기본 시스템 사용자 `system_default_user`를 생성한다.
- 테스트마다 독립적인 새 DB를 사용한다.

### Mock 사용 기준

실제 메모리 DB를 우선 사용하며, Mock은 테스트 대상과 무관한 의존성을 분리할 때 사용한다.

- 통합·E2E 테스트: 실제 Sqlite 구현체와 메모리 DB 사용.
- 단위 테스트: `MockBroadcaster`, `MockConversationRepo` 등으로 무관한 의존성 대체.
- Mock은 trait을 직접 구현하고 `Mutex<Vec<T>>` 등으로 메모리 데이터를 관리한다.

### E2E 테스트 패턴

`aionui-app/tests/common/mod.rs`에서 공통 테스트 도구를 제공한다.

```rust
// Build the complete application
let (app, services) = build_app().await;

// Create a user and log in
let (token, csrf) = setup_and_login(&services, "testuser", "password").await;

// Make an authenticated request
let response = app.oneshot(
    get_with_token("/api/conversations", &token, &csrf)
).await;
```

테스트의 로그인 순서는 다음과 같다.

1. API를 거치지 않고 Repository로 테스트 사용자를 생성한다.
2. `GET /api/auth/status`에서 CSRF 토큰을 가져온다.
3. `POST /login`으로 세션 토큰을 받는다.
4. 이후 요청에 `Authorization: Bearer`와 `x-csrf-token` 헤더를 보낸다.

### 테스트 파일 이름

| 접미사 | 용도 | 예시 |
|--------|------|------|
| `*_test.rs` | 단위·기능 테스트 | `extension_loading_test.rs` |
| `*_integration.rs` | 통합 테스트 | `acp_agent_integration.rs` |
| `*_e2e.rs` | 전체 흐름 테스트 | `auth_e2e.rs`, `conversation_e2e.rs` |

### 테스트 실패 처리

테스트를 통과시키기 위해 검증 조건부터 바꾸지 않는다. 먼저 실패 원인을 구분한다.

1. 기존 검증 조건이 여전히 올바르면 구현을 수정한다.
2. 요구사항이나 인터페이스가 의도적으로 바뀌었다면 테스트를 수정할 수 있다. 의도한 변경인지 확인하고, 새 검증 조건도 의미 있는 동작을 검증해야 한다.
3. 판단이 불확실하면 변경 내용을 추적하고 기대 동작을 확인한다.

실패한 테스트 삭제나 검증 완화로 문제를 숨기지 않는다. 예를 들어 `assert_eq!(status, 201)`을 `assert!(status.is_success())`로 바꾸면 정확한 상태 코드 검증이 사라진다.

## 새 기능 추가

### 새 패키지가 필요한 경우

자체 데이터 모델·수명 주기·라우트 접두사를 가진 독립적인 기능이면 새 패키지를 만든다. 기존 기능의 하위 기능이며 같은 모델과 경로를 공유한다면 기존 패키지를 확장한다.

### 새 도메인 패키지 추가 순서

`aionui-my-feature`를 추가하는 예시다.

**1. 패키지 생성과 workspace 등록**

`crates/aionui-my-feature/` 디렉터리를 만들고 루트 `Cargo.toml`의 workspace에 추가한다.

```toml
members = [
    # ... existing members
    "crates/aionui-my-feature",
]
```

같은 파일의 `[workspace.dependencies]`에도 등록한다.

```toml
aionui-my-feature = { path = "crates/aionui-my-feature" }
```

패키지 내부에서 공통 의존성을 사용할 때 `.workspace = true`로 workspace의 버전을 따른다.

**2. 표준 구조에 맞게 작성**

```text
crates/aionui-my-feature/
├── Cargo.toml
├── src/
│   ├── lib.rs        # 라우터·서비스·상태 타입 공개
│   ├── routes.rs     # my_feature_routes(state: ...) -> Router
│   ├── service.rs    # 비즈니스 로직
│   └── state.rs      # Clone을 구현하는 MyFeatureRouterState
└── tests/
    └── my_feature_test.rs
```

**3. DB 접근이 필요하면 `aionui-db`에 추가**

- `models/`에 Row 모델을 추가한다.
- `repository/`에 `I` 접두사의 trait과 `Sqlite` 접두사의 구현체를 추가한다.
- `migrations/`에 `NNN_descriptive_name.sql`을 추가한다.

**4. API 타입이 필요하면 `aionui-api-types`에 추가**

요청·응답 타입을 공통 패키지에 정의해 API 계약을 한곳에서 관리한다.

**5. `aionui-app`에 연결**

`aionui-app/Cargo.toml`에 의존성을 추가한다.

```toml
aionui-my-feature.workspace = true
```

`ModuleStates`에 상태 필드를 추가한다.

```rust
pub my_feature: MyFeatureRouterState,
```

Repository와 서비스를 구성하는 `build_my_feature_state()`를 작성한다.

```rust
pub fn build_my_feature_state(services: &AppServices) -> MyFeatureRouterState {
    let pool = services.database.pool().clone();
    let repo = Arc::new(SqliteMyFeatureRepository::new(pool));
    MyFeatureRouterState {
        my_feature_service: MyFeatureService::new(repo, services.event_bus.clone()),
    }
}
```

`build_module_states()`에서 해당 함수를 호출한다.

```rust
my_feature: build_my_feature_state(services),
```

`create_router_with_all_state()`에서 인증이 필요한 라우트에 미들웨어를 적용하고 병합한다.

```rust
let my_feature_authenticated = my_feature_routes(states.my_feature)
    .route_layer(from_fn_with_state(auth_mw_state.clone(), auth_middleware));

let router = Router::new()
    // ... existing routes
    .merge(my_feature_authenticated)
    // ...
```

### 확인 목록

- [ ] 패키지 내부가 lib/routes/service/state 구조를 따르는가?
- [ ] 상위 계층이나 같은 계층의 구체적인 구현체에 의존하지 않는가?
- [ ] Repository trait을 `aionui-db`에 정의하고 구현체에 `Sqlite` 접두사를 사용했는가?
- [ ] API 타입을 `aionui-api-types`에 정의했는가?
- [ ] 라우트에 `/api/` 접두사와 kebab-case 리소스 이름을 사용했는가?
- [ ] 해당 기능의 테스트가 있는가?
- [ ] WebSocket 이벤트가 `domain.camelCaseAction` 규칙을 따르는가?

## 실행 환경

### 관리형 Node 런타임

내장 ACP 어댑터는 `crates/aionui-runtime/src/node_runtime/`의 관리형 Node 런타임으로 실행한다.

- 패키징된 빌드는 managed-resources 번들에 포함된 Node를 사용한다.
- 다운로드 모드의 빌드는 지정된 버전의 Node를 `{data_dir}/runtime/node`에 설치한다.
- 어댑터 명령은 Node 실행 파일의 경로를 명시하므로, 시스템 `PATH`에서 우연히 발견되는 Node에 의존하지 않는다.

### 시작 시 PATH 보완

[실행 진입점](./crates/aionui-app/src/main.rs)의 `run_main()`은 Tokio 런타임을 만들기 전에 `aionui_runtime::enhance_process_path()`를 호출한다. 이후의 `which::which(...)`와 `Command::new(...)`는 보완된 PATH를 사용한다.

다음 순서로 경로를 합친다.

1. 대화형 로그인 셸의 `$PATH`. Unix에서는 3초 제한을 둔다.
2. 현재 프로세스가 상속받은 PATH.
3. 플랫폼별 기본 후보 경로. 예: `~/.cargo/bin`, `~/.local/bin`, 버전 관리 도구의 설치 경로, Windows의 `%APPDATA%\npm`, Git, Scoop.

Rust 2024의 `env::set_var`는 단일 스레드 실행 조건이 필요하므로 호출이 `unsafe`다. 따라서 작업 스레드와 Tokio 런타임을 만들기 전에 PATH를 설정한다.

`startup: PATH ready path_segments=… path_len=…` 로그로 적용 여부를 확인할 수 있다. `info` 수준에서는 전체 PATH 내용을 기록하지 않는다.

### 하위 프로세스 실행 Builder

하위 프로세스를 추가로 실행할 때는 공통 Builder를 사용한다.

| 생성자 | 용도 |
|--------|------|
| `aionui_runtime::Builder::agent(program)` | 오래 실행되며 호출자가 표준 입출력을 관리하는 에이전트 CLI |
| `aionui_runtime::Builder::clean_cli(program)` | 짧게 실행하고 결과를 파싱하는 도구 |

두 생성자는 모두 `kill_on_drop(true)`를 설정하고 `NODE_OPTIONS`, `NODE_INSPECT`, `NODE_DEBUG`, `CLAUDECODE`를 제거해 상위 프로세스의 디버깅 환경 등이 전달되지 않도록 한다.

`clean_cli`는 표준 입출력을 파이프로 연결하고 `NO_COLOR=1`, `TERM=dumb`을 설정해 수집한 출력에 ANSI 제어 코드가 섞이지 않도록 한다.

이 동작을 `tokio::process::Command`로 각자 다시 구현하지 않는다. 실행 정책을 공통 Builder에 모아야 환경변수 정리나 실행 옵션을 일관되게 변경할 수 있다.
