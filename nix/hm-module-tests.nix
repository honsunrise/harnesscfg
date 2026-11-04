{
  lib,
  pkgs,
  module,
}:
let
  # Exercise the module's real option types and assertions. Home Manager owns
  # these outer namespaces; their activation values are deliberately not forced.
  evaluate =
    file:
    (lib.evalModules {
      specialArgs = { inherit pkgs; };
      modules = [
        module
        {
          options = {
            assertions = lib.mkOption { type = lib.types.listOf lib.types.attrs; };
            home = lib.mkOption { type = lib.types.lazyAttrsOf lib.types.raw; };
            xdg = lib.mkOption { type = lib.types.lazyAttrsOf lib.types.raw; };
          };
          config = {
            home.homeDirectory = "/home/test";
            xdg = {
              configHome = "/home/test/.config";
              dataHome = "/home/test/.local/share";
              stateHome = "/home/test/.local/state";
            };
            programs.harnesscfg = {
              enable = true;
              files."custom/settings" = {
                source = builtins.toFile "harnesscfg-test-template.json" "{}";
                target = "/home/test/settings";
                seed = "copy";
              }
              // file;
            };
          };
        }
      ];
    }).config.assertions;

  accepts =
    file:
    let
      failures = lib.filter (item: !item.assertion) (evaluate file);
    in
    # An invalid list must produce ordinary assertion diagnostics, not another
    # type error while Home Manager tries to render its messages.
    builtins.deepSeq (map (item: item.message) failures) (failures == [ ]);

  patch = {
    kind = "patch";
    file = "settings.json";
    format = "json";
  };
  preset = {
    kind = "preset";
  };
  enabled = {
    kind = "enabled";
  };
  cases = [
    {
      format = null;
      sync = null;
      expected = true;
    }
    {
      format = null;
      sync = [ ];
      expected = true;
    }
    {
      format = null;
      sync = patch;
      expected = true;
    }
    {
      format = null;
      sync = [
        patch
        (patch // { file = "models.json"; })
      ];
      expected = true;
    }
    {
      format = "json";
      sync = preset;
      expected = true;
    }
    {
      format = "yaml";
      sync = [
        enabled
        preset
      ];
      expected = true;
    }
    {
      format = null;
      sync = preset;
      expected = false;
    }
    {
      format = null;
      sync = [ preset ];
      expected = false;
    }
    {
      format = null;
      sync = [
        patch
        enabled
      ];
      expected = false;
    }
  ];
in
assert lib.all (case: accepts (removeAttrs case [ "expected" ]) == case.expected) cases;
pkgs.runCommand "harnesscfg-hm-module-tests" { } ''
  touch "$out"
''
