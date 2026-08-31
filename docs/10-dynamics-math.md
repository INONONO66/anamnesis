# 10 — 역학 수식 스펙 카드

> docs/09(우주 모델)의 역학을 검증된 문헌 기반 수식으로 고정한다.
> 카드 규약은 omni-anamnesis math-conventions를 따른다: 기호표·단위 명시,
> 역방향(가역성) 3종, 실패 조건, 캘리브레이션 대상 명시, 매직 넘버 금지.
> 여기 수식이 코드의 정본이며, docs/09 §5의 요약은 이 문서를 따른다.

## 관례 (전 카드 공통)

- **시간 단위: days.** 저장은 ms epoch이지만 수식 투입 전 days로 변환한다.
  (Anki revlog의 days/seconds/ms 혼재 사고, donor의 ms/days 단위 버그 —
  두 프로젝트 모두에서 실제로 터진 사고라 단위 계약을 카드마다 명시한다.)
- **점수는 확률이 아니다.** PPR 값·RRF 값·m(T)는 사후확률이 아니며 확률과
  수식으로 섞지 않는다 (카드 3 불변식).
- **매직 넘버 금지.** 모든 파라미터는 (a) 문헌 피팅값 인용 또는 (b) 우리
  히트 원장에서 재피팅 가능한 캘리브레이션 대상으로 표기한다.

## 이론적 지붕 — 기억은 "필요 확률" 추정기다

Anderson & Schooler의 합리적 분석(1991): 인간 기억의 recency·frequency
법칙은 결함이 아니라 **환경 통계 — 어떤 정보가 다시 필요해질 확률 — 의
최적 반영**이다. NYT 헤드라인·이메일 로그의 재등장 통계가 망각 곡선과
같은 멱법칙을 따른다.
(https://journals.sagepub.com/doi/abs/10.1111/j.1467-9280.1991.tb00174.x)

이 프레임이 전체 설계의 지붕이다:

- 카드 5의 `score = rel × m(T)^γ`는 휴리스틱이 아니라 **필요 확률 =
  문맥 연관성 × 사전 필요 오즈**의 구현이다. rel이 문맥 항, m(T)가
  사전 오즈 항.
- 캘리브레이션의 원리적 근거: 히트 원장은 곧 "이 사용자의 환경 통계"다.
  DECAY·S_base를 원장에서 재피팅하는 건 튜닝이 아니라 이론이 요구하는
  행위다.

---

## 카드 1 — 망각 곡선 m(T): 지수 → 멱법칙 (FSRS 계약)

**지위: 확정 제안.** docs/09 §5의 `exp(−t/S)`를 대체한다.

### 왜 지수가 아닌가

- 심리학 100년 합의: 망각은 지수가 아니라 **멱법칙**이다. Ebbinghaus 재현
  연구들과 Wixted & Ebbesen "On the Form of Forgetting"(1991), PNAS 리뷰
  "The enigma of forgetting"이 정리한 power law of forgetting.
  (https://www.pnas.org/doi/10.1073/pnas.2201332119)
- 공학적 근거: open-spaced-repetition **srs-benchmark**(3.5억 리뷰) —
  멱형 곡선의 FSRS-6가 LogLoss 0.346 / AUC 0.703으로 지수·ACT-R 계열을
  일관되게 이긴다. 함수형 선택이 벤치로 판정된 드문 사례.
  (https://github.com/open-spaced-repetition/srs-benchmark)
- 직관: 지수는 "오래된 것은 결국 다 죽는다". 멱법칙은 초반에 빨리 잊고
  꼬리가 두껍다 — 몇 달 지난 기억도 정면 질의에 걸릴 잔존치를 유지한다.
  개인 장기기억 엔진에 필요한 건 후자다.

### 정방향

```text
t        = T − t_last_hit                      # days
R(t, S)  = (1 + FACTOR · t / S)^DECAY          # 잔존율 ∈ (0, 1]
m(T)     = m₀ · R(t, S)

DECAY    = −0.5            # 캘리브레이션 대상 (FSRS-6는 이것도 학습 파라미터)
FACTOR   = 0.9^(1/DECAY) − 1 = 19/81           # R(S, S) = 0.9 고정 규약
```

**기호표**: t = 마지막 강화 후 경과(days); S = 안정도(days) — "잔존율이
90%로 떨어지기까지 걸리는 기간"이라는 물리적 의미를 가진다; m₀ = 고유
질량(0~1, 탄생 시 부여, 불변, docs/01); DECAY = 곡선 평탄도.
FACTOR는 자유 파라미터가 아니라 R(S,S)=0.9를 강제하는 종속값이다 —
DECAY를 바꾸면 FACTOR가 따라 바뀐다.
(수식 출처: FSRS-4.5+/FSRS-6 forgetting curve,
https://expertium.github.io/Algorithm.html ,
https://github.com/open-spaced-repetition/awesome-fsrs/wiki/The-Algorithm)

### 역방향 (가역성 3종)

- (a) 해석적 역: 목표 잔존율 R*까지 남은 시간 **t = (S/FACTOR)·(R*^(1/DECAY) − 1)**.
  dreaming이 "이 먼지가 사실상 식는 시점"을 스캔 없이 계산할 때 쓴다.
- (b) 피팅: DECAY(및 카드 2의 a, b, c)는 히트 원장에 대한 이진 log-loss
  MLE. 프로토콜은 srs-benchmark 그대로: TimeSeriesSplit, 과거→학습
  미래→평가, 1차 지표 LogLoss.
- (c) 재생: 히트 원장(카드 2)에서 (S, t_last_hit) 궤적 전체를 결정론적으로
  재생 가능 — CREATE-only 원칙과 정합.

### 실패 조건

- t < 0 (미래 시각 히트): 입력 거부. 시계 역행은 상위에서 차단.
- S ≤ 0: 정의역 밖. S_min = 1일 하한 클램프.
- DECAY ≥ 0: 곡선이 증가함수가 되어 무의미. DECAY ∈ [−2, −0.1] 범위 강제.
- **CI 픽스처**: R(S,S) = 0.9 정확 일치, R(0,S) = 1, 단조 감소성.

---

## 카드 2 — 안정도 갱신 S: 선형 누적 → 포화 간격 효과

**지위: 확정 제안.** docs/09 §5의 `S = S_base·(1 + α·hits)`를 대체한다.

### 왜 선형 누적이 아닌가

선형식은 히트 횟수만 세고 **간격을 보지 않는다.** 같은 세션에서 10번
연속 노출(massed)과 한 달 간격 10번 노출(spaced)이 같은 S를 받는다.
간격 효과(spacing effect)는 인지과학에서 가장 재현이 잘 되는 현상이고,
Pavlik & Anderson 2005(ACT-R 다중흔적)와 FSRS의 안정도 증가식이 공통으로
포착하는 구조는 하나다: **회수가 어려웠을수록(잔존율이 낮은 시점의 히트일수록)
강화가 크다.**

### 정방향 — 히트 시점 갱신

```text
탄생:   S₀ = S_base(레벨) · (1 + λ · m₀)

히트(시각 t_hit, 종류 kind):
  R_hit = R(t_hit − t_last_hit, S)             # 그 순간의 잔존율 (카드 1)
  S′    = S · ( 1 + a · κ(kind) · (e^{b·(1−R_hit)} − 1) · S^{−c} )
  t_last_hit ← t_hit

κ(recall_hit) = 1.0   # 질의가 능동적으로 꺼냄 = 회수(retrieval)
κ(re_mention) = 0.5   # 사용자가 다시 말함 = 재노출(restudy)
κ(promotion)  = 0.3   # dreaming 내부 사용
```

**테스팅 효과(κ의 근거)**: 회수는 재노출보다 장기 보존을 확연히 더 크게
강화한다 — 1주 후 61% vs 40% (Roediger & Karpicke 2006,
https://journals.sagepub.com/doi/10.1111/j.1467-9280.2006.01693.x).
그래서 히트 종류는 원장 메타데이터가 아니라 강화식의 입력이다.
κ 벡터는 캘리브레이션 대상.

**기호표**: S_base(레벨) = 먼지 1일 / 행성 30일 / 엔티티 ∞ (풍화 없음) —
캘리브레이션 대상; λ = 고유 질량의 초기 안정도 기여 (기본 1.0);
κ(kind) = 히트 종류별 강화 계수 (위);
a = 강화 스케일, b = 간격 민감도, c ∈ [0,1) = 포화 지수 — 전부
캘리브레이션 대상 (FSRS-6 대응 항: e^{w8}, w10, w9).

구조는 FSRS 안정도 증가식 `S′ = S·(1 + e^{w8}·(11−D)·S^{−w9}·(e^{w10·(1−R)}−1))`
에서 온다 (https://expertium.github.io/Algorithm.html). 단 FSRS의 입력은
4단계 등급(Again/Hard/Good/Easy)인데 **우리 히트 신호는 이진**(회상 적중 /
재언급 / 승격)이다 — omni 카드 2가 지적한 "곡선(채택)과 (S,D) 갱신
법칙(도메인 적응)의 분리"가 정확히 이 지점이고, 위 식은 등급 항을 제거한
이진 적응이다. 난이도 D의 자리는 m₀가 맡는다 (중요한 기억 = 낮은 난이도
= 큰 S₀).

### 이 식이 주는 성질

- **간격 효과**: 오래 안 꺼내다(R_hit 낮음) 꺼내면 e^{b·(1−R)} 항이 커서
  S가 크게 뛴다. 방금 꺼낸 걸 또 꺼내면(R_hit ≈ 1) 강화 ≈ 0 —
  같은 세션 연속 노출로 S를 부풀릴 수 없다 (massed 재노출 무력화).
- **포화**: S^{−c} 항이 커진 S의 추가 강화를 눌러 S 폭주를 막는다.
- **부활**: m(T) → 0으로 식은 먼지도 히트 한 번이면 t_last_hit 리셋 +
  최대폭 강화로 살아난다 (망각 ≠ 삭제와 정합).

### 원장 계약 (Neo4j 통합)

Pavlik-Anderson의 교훈(omni 카드 1): 정확한 역학 평가에는 **전체 히트
타임스탬프 이력이 필요하며 고정 크기 충분통계량이 없다.** 따라서:

- **정본 = 히트 원장.** `(:Hit {element_id, t_utc, kind})` CREATE-only.
  kind ∈ {recall_hit, re_mention, promotion}. expendable 아님 —
  원소·링크와 같은 급의 보존 대상이다 (유실되면 m(T)가 비가역 손실).
- **캐시 = (S, t_last_hit, hit_count) 노드 프로퍼티.** 원장에서 언제든
  재생. scores 캐시와 같은 취급.
- 읽기 시점 m(T) 평가는 캐시만 읽는다 — recall 경로에 원장 스캔 없음.

### 실패 조건

- 동일 recall 안에서 같은 원소 다중 히트: 1회로 병합 (t_i → 0 발산의
  P-A 실패 조건 대응).
- 히트 남발 (모든 recall 결과를 히트 처리): 상위 k개 + 실제 컨텍스트
  채택분만 히트로 커밋. "노출"이 아니라 "사용"이 강화다.
- **CI 픽스처**: spaced > massed (동일 히트 수, 간격만 다를 때 S′ 비교),
  S 단조 증가, c > 0에서 상방 포화.

---

## 카드 3 — PPR: 컨벤션·수렴·불변식 (omni ppr-spec 승계)

**지위: 확정.** omni ppr-spec의 검증분을 그대로 승계하고 Neo4j GDS에
바인딩한다.

### 컨벤션 고정 (의무)

문헌에 α 의미가 정반대인 두 정식화(Jeh-Widom damping vs ACL restart)가
공존한다. **우리 표준: `p = (1−α)·s + α·Wᵀp`, α = 워크 지속 확률
(damping) = 0.85, s = 재시작 분포.** ACL 계열(Forward Push) 인용 시
재사상 명시.

- Neo4j GDS 바인딩: `gds.pageRank`의 `dampingFactor` = 우리 α와 동일
  컨벤션(워크 지속). `sourceNodes` = s. 그대로 매핑된다.
- **수렴 상한**: 오차 ≤ α^k. τ=10⁻⁶, α=0.85 → k ≈ 85. GDS 설정
  `maxIterations: 100, tolerance: 1e-6` — 상한이 수학으로 보장되는
  CI 체크 대상.
- **조건수**: κ = (1+α)/(1−α), α→1에서 폭주. α 캘리브레이션 상한 0.95.
- **dangling 계약**: 출차수 0 노드는 제거하지 않고 rank-one fix(재시작
  벡터로 치환). GDS PageRank가 내부 처리하지만, 커스텀 확산으로 갈아탈
  때 이 계약을 유지한다.

### 불변식 두 개

1. **PPR은 사후확률이 아니다.** 점수는 "확률적 그래프 관련도"로만 서술.
   semantic 유사도·m(T)와 가법 혼합 금지 → 융합은 카드 5의 RRF로만.
2. **선형성 정리 (Jeh-Widom, 정확)**: 재시작 혼합 β₁u₁+β₂u₂의 PPV =
   β₁v₁+β₂v₂ 정확 분해. 시드를 "질의 질량 + 최근 세션 질량 + 항성(정체성)
   질량"으로 혼합해도 기여분을 사후 분리할 수 있고, 섭동이 ‖Δs‖₁로
   선형 유계 — "개인화는 순위만 바꾼다"의 정량형.

### 시딩 — HippoRAG 2 반영

HippoRAG(NeurIPS'24)·HippoRAG 2(ICML'25)의 검증된 레시피를 우리 층에
매핑한다 (https://arxiv.org/abs/2405.14831 ,
https://arxiv.org/abs/2502.14802):

- **이중 노드 확산**: HippoRAG 2의 phrase node + passage node 통합은
  우리 엔티티(2층) + 에피소드/사실(1·3층) 혼합 시딩에 대응. 엔티티만
  시드로 쓰지 말고 벡터 상위 원소 노드도 소량 질량으로 시드에 포함 —
  entity-free 질의(시맨틱 질의)에서의 붕괴를 막는다.
- **node specificity**: 시드 질량에 희귀도 가중 w(n) ∝ 1/deg(n)
  (HippoRAG의 IDF 근사). "커피" 같은 허브 엔티티가 확산을 지배하는 것을
  차단 — graphiti 소음 엔티티 교훈(docs/08)과 같은 문제의 수학적 해법.
- **재시작 혼합 기본값**: 질의 시드 0.7 / 최근 세션 0.2 / 항성 프라이어
  0.1 (캘리브레이션 대상; 선형성 정리로 사후 조정 가능).
  "최근 세션 질량"의 인지적 근거는 **부호화 특수성**(Tulving 계열:
  회상은 단서와 부호화 문맥의 일치가 결정) — 현재 대화 문맥은 그 자체가
  회수 단서다. v0.2: 에피소드에 세션 문맥 임베딩을 함께 저장해두면
  문맥 일치 채널을 RRF 4번째 채널로 열 수 있다.
- **팬 효과의 구현**: 허브 노드의 연결이 많을수록 개별 연상이 약해지는
  팬 효과(Anderson 1974)는 PPR의 행확률 정규화(출차수로 질량 분할) +
  node specificity가 수학적으로 동일한 일을 한다. 별도 구현 불필요.

### 전파 규약 (role별)

| role | 전파 | 비고 |
|---|---|---|
| sequence / about / semantic / provenance | ○ (role별 가중 캘리브레이션) | |
| invalidates | × | 판정 링크. 활성화를 나르면 죽은 사실이 되살아난다 |
| contrasts | × | 모순 링크는 전도체가 아니다 (main의 Contradicts 배제 승계). 양끝이 독립적으로 활성일 때만 함께 노출 |

---

## 카드 4 — 고유 질량 m₀: 서프라이즈 부호화

**지위: 제안 (v0.2).** docs/01의 "LLM이 한 번 평가"를 구조화한다.

신경과학 근거: 부호화 강도는 예측 오차(surprise)가 결정한다 — 해마는
예측 위반 시 기억 갱신 모드로 전환하고(Sinclair et al., PNAS 2021,
https://www.pnas.org/doi/10.1073/pnas.2117625118), LC-해마 회로가
surprise를 부호화 게인으로 변환한다 (Trends in Neurosciences 2025,
https://www.sciencedirect.com/science/article/pii/S0166223625001894).
main의 "encoding surprise → 감쇠 면제 prior P_i" 설계가 같은 직관이다.

```text
m₀ = σ( β_imp · importance + β_nov · novelty + β_aro · arousal + β₀ )

importance ∈ [0,1]  : digest 시 LLM 1회 평가 (현행)
novelty    ∈ [0,1]  : 1 − max_{k-NN} cos(e_new, e_k)   # 임베딩 kNN — LLM 0
arousal    ∈ [0,1]  : 정서 각성 — importance와 같은 digest LLM 호출에서 함께 평가
```

- novelty는 Neo4j 벡터 인덱스 kNN 한 번으로 계산 — digest 경로에 이미
  있는 인덱스를 재사용하며 LLM 호출이 늘지 않는다.
- "이미 아는 얘기의 재탕"은 m₀가 낮게 태어나고, 대신 재언급 히트(카드 2)로
  기존 원소의 S를 강화한다 — 신규 생성 vs 기존 강화의 분업.
- novelty 항은 고립 효과(von Restorff — 특이한 것이 잘 남는다)의 구현이기도 하다.
- arousal 항의 근거는 정서 각성의 기억 응고 변조(McGaugh 편도체 변조 계열) —
  개인 기억에서 감정적으로 격한 에피소드는 중요도·novelty와 독립인 보존
  신호다. **핀 인용 미확보(미검증 표기)** — 규범 인용 전 McGaugh 2004 등
  원본 대조 필요.
- β 벡터는 독립 노브가 아니라 하나의 캘리브레이션 회귀 (main ADR-0010
  "calibrated priors, not laws" 승계).

---

## 카드 5 — 재정렬 융합: 가법 혼합 → RRF × m(T)

**지위: 확정 제안.** docs/03의 `α·semantic + β·temporal + γ·graph + δ·entity
+ mass 보정`을 대체한다.

### 왜 가법 혼합이 아닌가

vector 유사도(코사인), BM25(비유계), PPR(그래프 크기에 따라 수축하는
정상분포 질량)은 **스케일이 서로 비교 불가능**하다. 가법 혼합은 카드 3
불변식 1 위반이며, graphiti가 실전에서 RRF로 후퇴한 이유이기도 하다
(docs/08). 랭크 기반 융합은 스케일 문제를 정의상 제거한다.

### 정방향

```text
1단계 — 관련도 융합 (rank 공간):
  rel(e) = Σ_{c ∈ {vector, bm25, ppr}} w_c / (k + rank_c(e))     # RRF, k = 60

2단계 — 질량 가중 (곱셈, hermes ADR-102):
  score(e) = rel(e) · m(T)^γ ,   γ ∈ (0, 1]  기본 0.5
```

- **곱셈이지 필터가 아니다**: m(T)가 낮아도 rel이 압도적이면 노출된다
  (식은 먼지의 정면 지목 응답). hermes가 54% 오판정 사고 후 도달한 결론과
  MemoryBank(Ebbinghaus 감쇠를 검색 점수에 곱하는 구조,
  https://www.emergentmind.com/papers/2305.10250)의 공통 형태.
- γ는 질량의 랭킹 영향력 완충 지수. γ=0이면 질량 무시(순수 관련도),
  γ=1이면 전면 반영. 질의 유형 프로파일(사실 조회 vs 회고)은 α..δ가
  아니라 w_c(채널 가중)와 γ로 표현한다 — 노브 수가 줄고 전부 rank 공간.
- **이론적 근거**: 이 곱셈 구조는 합리적 분석(서두 지붕)의 필요 확률
  분해 — 문맥 연관성(rel) × 사전 필요 오즈(m(T)) — 그대로다.
- temporal 근접성이 필요한 질의(시간 추론)는 별도 채널이 아니라 snapshot(T)
  절단 + sequence 링크를 타는 PPR이 이미 나른다. 부족이 실측되면 시간
  근접 rank 채널을 RRF에 4번째로 추가 — 구조 변경 없음.

### 실패 조건

- 채널 하나가 빈 결과: 해당 항 0 (RRF는 결측에 자연 강건).
- **CI 픽스처**: 스케일 불변성 (임의 채널 점수를 단조 변환해도 순위 불변),
  m(T)=0 원소가 압도적 rel로 여전히 상위 노출 가능.

---

## 카드 6 — dreaming 재정규화: 수면의 두 기능

**지위: 방향 확정, 수식은 v0.2.**

수면 신경과학의 두 축을 dreaming의 두 작업에 대응시킨다:

1. **시스템 통합 (해마→신피질)**: 반복 패턴의 먼지에서 사실(행성)을
   추출·승격. HippoRAG의 neocortex/hippocampus 분업 프레이밍과 동일 —
   승격은 카드 2의 promotion 히트로 기록되어 원본 먼지도 강화된다.
2. **시냅스 재정규화 (SHY, Tononi & Cirelli 2014, Neuron,
   https://www.cell.com/neuron/fulltext/S0896-6273(13)01186-0)**:
   깨어 있는 동안 순증가한 연결 강도를 수면이 일괄 하향 재정규화해
   신호 대 잡음을 회복한다. 우리 대응: dreaming이 **링크를 만드는 만큼
   정리한다** — 중복 semantic 링크 병합, contrasts의 invalidates 승격,
   저질량 고아 링크 감쇠. 원소 질량의 재정규화는 불필요하다 (감쇠가
   읽기 시점 수식으로 이미 연속 작동 — tick 없는 설계의 이점).

### 재응고화 — 히트 직후가 판정의 창

회수된 기억은 일시적으로 불안정(labile) 상태가 되어 그 창 안에서 갱신된다
(Nader 계열 reconsolidation,
https://www.sciencedirect.com/science/article/pii/S1364661317300785).
우리 대응은 수식이 아니라 **판정 우선순위 규칙**이다:

- digest의 모순 후보 대조는 전체 그래프가 아니라 **직전 recall에서
  히트된 사실들**부터 본다 — 새 발화가 방금 꺼낸 기억과 충돌할 확률이
  가장 높다.
- dreaming의 contrasts → invalidates 승격 큐도 최근 히트된 모순부터
  처리한다.

판정 비용을 줄이면서 인지적으로 올바른 순서다.

항성 중력(docs/09 §3)은 그대로: 엔티티 중력 = Σ 궤도 원소 m(T).
이는 1-step 확산의 근사이므로 별도 수식이 필요 없고, PPR 항성 프라이어
시드(카드 3)의 질량 배분에 쓴다.

---

## 구현 금지 목록

- **회수 유발 망각(RIF)의 명시 구현 금지.** 히트가 경쟁 기억을 능동
  억제하는 현상은 실재하지만(Anderson 계열,
  https://www.nature.com/articles/nn.3973), 엔진에 명시 억제를 넣으면
  "쓸수록 옆 기억이 지워지는" 사용자 적대적 동작이 된다. 랭킹이 상대
  경쟁이므로 승자 강화 = 경쟁자 상대 순위 하락, 즉 RIF의 적응적 기능은
  이미 암묵적으로 발생한다. 그 이상은 금지.

---

## 캘리브레이션 대상 총목록

| 파라미터 | 카드 | 기본값 | 피팅 원료 |
|---|---|---|---|
| DECAY | 1 | −0.5 | 히트 원장 log-loss MLE |
| S_base(레벨), λ | 2 | 1d / 30d / ∞, 1.0 | 〃 |
| a, b, c | 2 | 문헌 대응 초기값에서 시작 | 〃 |
| κ(kind) | 2 | 1.0 / 0.5 / 0.3 | 〃 |
| α (damping) | 3 | 0.85 (상한 0.95) | 회상 채택 라벨 |
| 시드 혼합 β 벡터 | 3 | 0.7/0.2/0.1 | 〃 |
| role 전파 가중 | 3 | 균등 | 〃 |
| β_imp, β_nov, β_aro | 4 | 회귀 피팅 전 0.6/0.25/0.15 | digest 후 재접근율 |
| w_c, k, γ | 5 | 균등, 60, 0.5 | 회상 채택 라벨 |

전부 "calibrated priors, not laws" (main ADR-0010): 초기값은 문헌
인용값이고, 우리 원장이 쌓이면 srs-benchmark 프로토콜(TimeSeriesSplit,
LogLoss)로 재피팅한다.
