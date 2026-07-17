//! scaffolder — 선언형 프로젝트 스캐폴딩 CLI.
//!
//! 레이어드 아키텍처: `cli → app/infra → domain`.
//! domain은 순수(무의존)하며 포트 트레잇을 정의하고, infra가 포트를 구현하며,
//! app이 도메인 포트만으로 라이프사이클을 조립하고, cli가 합성 루트다.

pub mod domain;
pub mod app;
pub mod infra;
pub mod cli;
