Feature: Extension archive installation
  Scenario: Install a fixture archive into a scratch tree
    Given a scratch PostgreSQL tree and a manifest describing a fixture archive
    When the declared extensions are installed
    Then the three fixture files exist with library and share modes
    And the report lists the fixture extension with three files

  Scenario: Reinstalling the same archive is a reporting no-op
    Given a scratch PostgreSQL tree and a manifest describing a fixture archive
    When the declared extensions are installed
    And the declared extensions are installed again
    Then the fixture files are unchanged
    And the report lists the fixture extension with three files

  Scenario: An archive whose digest disagrees with the manifest is refused
    Given a scratch PostgreSQL tree and a manifest describing a fixture archive
    And the manifest records the wrong archive digest
    When the declared extensions are installed
    Then the install fails with kind ExtensionArchiveUnavailable
    And the scratch tree is untouched

  Scenario: An unknown extension name is refused
    Given a scratch PostgreSQL tree and a manifest describing a fixture archive
    And the request also names an extension the manifest lacks
    When the declared extensions are installed
    Then the install fails with kind ExtensionUnavailable

  Scenario: A missing manifest is refused
    Given a scratch PostgreSQL tree and a manifest describing a fixture archive
    And the manifest file is removed
    When the declared extensions are installed
    Then the install fails with kind ExtensionManifestUnavailable
    And the scratch tree is untouched

  Scenario: An archive entry that escapes its prefix is refused
    Given a scratch PostgreSQL tree and a manifest describing a fixture archive
    And the archive gains an entry that escapes to the parent directory
    When the declared extensions are installed
    Then the install fails with kind ExtensionArchiveInvalid
    And the scratch tree is untouched
