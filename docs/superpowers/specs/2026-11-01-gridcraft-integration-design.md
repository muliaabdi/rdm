# RDM — GridCraft Integration

## Intent & Why

Mengadopsi struktur & best practices dari GridCraft untuk meningkatkan kualitas, keandalan, dan maintainability RDM (Rust Download Manager). Tujuan:

- Meminimalisir crash dan panic
- Memastikan error handling yang konsisten
- Meningkatkan dokumentasi & traceability
- Meningkatkan test coverage & regression testing
- Meningkatkan CI/CD security & quality gates
- Membuat kontributor lebih mudah di-track

---

## Goals, Non-goals, Anti-goals

### Goals
- Struktur plan/ yang formal (STATUS, execution-plan, architecture)
- Rust linting strict (`#![deny(...)]`)
- Regression tests untuk error handling
- Test fixtures untuk yt-dlp/gallery-dl output
- Feature parity documentation
- Security audit & license compliance
- Contributor tracking

### Non-goals
- Mengubah arsitektur core (layering sudah OK)
- Mengubah UI (vanilla JS sudah OK)
- Mengubah deployment (install.sh sudah OK)
- Mengubah extension UX (sudah bekerja)

### Anti-goals
- Tidak menambah panic/unwrap/expect/todo/unreachable
- Tidak copy code dari aplikasi lain (IDM, dll)
- Tidak membuat dokumentasi yang tidak maintainable
- Tidak menambah overhead CI yang terlalu besar

---

## Constraints

- Deadline: sesi-sesi berurutan (bukan satu sesi)
- Resource: 1 developer (muliaabdi)
- Existing codebase: 4 crates (rdm-core, rdm-daemon, rdm-ui, rdm-host)
- CI: GitHub Actions dengan PAT workflow scope
- Target: v0.7.0 + backlog

---

## Approaches

### Approach 1: Phased Implementation (RECOMMENDED)

**Urutan:**
1. **Fase 1: Foundation** (1-2 sesi)
   - Plan structure (STATUS, execution-plan, architecture)
   - Rust linting strict
   - Documentation (feature-parity, gaps, architecture)
   - Quality gates (cargo audit, cargo deny)

2. **Fase 2: Testing** (1-2 sesi)
   - Regression tests untuk error handling
   - Test fixtures untuk yt-dlp/gallery-dl
   - Fuzz random command sequences
   - Test DB corrupt recovery

3. **Fase 3: CI/CD** (1 sesi)
   - Security audit job
   - License compliance check
   - Shared workflows (jika perlu)

4. **Fase 4: Contributor Tracking** (1 sesi)
   - contributors/contributors.json
   - contributors/people.toml
   - PR check

5. **Fase 5: Polish** (1 sesi)
   - Icon assets (SVG inline atau repo terpisah)
   - Clean-room documentation
   - Final review

**Kelebihan:**
- Bisa review setiap fase
- Bisa adjust berdasarkan feedback
- Risiko terkumpul per fase, bukan satu blok besar
- Bisa selesai dalam beberapa sesi

**Kekurangan:**
- Butuh waktu lebih lama (5 sesi)
- Perlu review antar fase

---

### Approach 2: Big Bang

**Deskripsi:**
Implementasi semua fase sekaligus dalam satu sesi panjang.

**Kelebihan:**
- Lebih cepat (1 sesi)
- Konsisten dari awal

**Kekurangan:**
- Risiko error terkumpul
- Sulit review setiap perubahan
- Sulit debug jika ada issue
- Mungkin melebihi token limit

---

### Approach 3: Prioritize Critical Only

**Deskripsi:**
Fokus hanya pada fase yang paling kritis:
- Error handling + regression tests
- Feature parity documentation
- Quality gates (cargo audit, cargo deny)

**Kelebihan:**
- Fokus pada high-impact items
- Lebih cepat (2-3 sesi)

**Kekurangan:**
- Documentation incomplete
- Contributor tracking tidak ada
- CI/CD tidak optimal

---

## Why Approach 1

Approach 1 dipilih karena:

1. **Manageable risk** — setiap fase bisa review & test secara terpisah
2. **Better feedback** — feedback lebih cepat setelah setiap fase
3. **Easier debugging** — jika ada issue, bisa identifikasi fase yang bermasalah
4. **Better documentation** — setiap fase bisa dokumentasikan dengan baik
5. **Can adjust** — bisa skip/adjust fase jika perlu

---

## What's Left to the Builder

Setelah approval:

1. **Create plan/ structure**
   - `plan/STATUS.md`
   - `plan/execution-plan.md`
   - `plan/architecture.md`

2. **Update Cargo.toml**
   - Tambah `#![deny(...)]` ke semua crates
   - Tambah `clippy` warnings untuk error handling

3. **Create docs/**
   - `docs/feature-parity.md`
   - `docs/gaps.md`
   - `docs/architecture.md`
   - `docs/extension-parity.md`
   - `docs/scheduler-parity.md`

4. **Update CI/CD**
   - Tambah security audit job
   - Tambah license compliance check

5. **Create test fixtures**
   - Mock yt-dlp/gallery-dl output
   - Mock socket failure
   - Mock DB corrupt

6. **Create contributor tracking**
   - `contributors/contributors.json`
   - `contributors/people.toml`

7. **Update AGENTS.md**
   - Tambah session start protocol
   - Tambah "Never crash" rule
   - Tambah non-negotiables

---

## Builder Check (Pending)

Ada beberapa pertanyaan yang perlu dijawab:

1. **Prioritas backlog** — backlog (tray speed toggle, clipboard import, dll) mana yang paling tinggi?
2. **Test coverage target** — berapa persen test coverage yang diharapkan?
3. **CI time limit** — berapa maksimal waktu CI per commit?
4. **Contributor tracking** — apakah RDM akan menerima kontributor dari open source?

---

## Implementation Notes

- Semua perubahan akan committed ke `main` branch
- Setiap perubahan akan di-test dengan `cargo test`
- Setiap perubahan akan di-review dengan `cargo clippy -D warnings`
- Setiap perubahan akan di-format dengan `cargo fmt`
- Semua dokumentasi akan ditulis dalam Bahasa Indonesia (user-facing)
