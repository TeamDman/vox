# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0-rc.6](https://github.com/bearcove/phon/compare/phon-schema-v0.2.0-rc.5...phon-schema-v0.2.0-rc.6) - 2026-08-20

### Added

- freeze canonical durable schemas
- add canonical schema bundle format
- freeze semantic names and schema encoding
- compare canonical schema bytes without allocation
- bound self-describing schema allocation

### Fixed

- reject unresolved TypeScript semantics

### Other

- adopt collision-safe Taxon identities
- restore standalone Phon workspace
