# F0-C IAR DWARF experiment

This experiment compiles and links an isolated Cortex-M4 fixture with IAR
8.32.3, then resolves fixed access plans from the linked DWARF and checks the
initialized ELF bytes. The fixture is never programmed to the target.

## Fixture build

```powershell
& "C:\Program Files (x86)\IAR Systems\Embedded Workbench 8.2\arm\bin\iccarm.exe" "experiments\p0\f0c-dwarf\fixture\F0cDwarfFixture.c" --debug --cpu Cortex-M4 --endian little -On -e --only_stdout -o "validation\evidence\f0-c\F0cDwarfFixture.o"
& "C:\Program Files (x86)\IAR Systems\Embedded Workbench 8.2\arm\bin\ilinkarm.exe" "validation\evidence\f0-c\F0cDwarfFixture.o" --config "D:\SVN\DCU\T26_DCU\trunk\03_code\T26_DCU_APP_NXP\Appl\LinkFile\S32K144_64_ram.icf" --no_entry --no_library_search --keep F0cDwarfFixtureHold --keep gstF0cRoot --keep gstF0cFlex --keep gaucF0cFlexPayload --keep gaunF0cFloatSpecial --keep gaunF0cDoubleSpecial --map "validation\evidence\f0-c\F0cDwarfFixture.map" -o "validation\evidence\f0-c\F0cDwarfFixture.out"
```

## Parser verification

```powershell
& "C:\Users\usre\.cargo\bin\cargo.exe" test --manifest-path "experiments\p0\Cargo.toml" -p f0c-dwarf
& "C:\Users\usre\.cargo\bin\cargo.exe" clippy --manifest-path "experiments\p0\Cargo.toml" -p f0c-dwarf --all-targets -- -D warnings
& "C:\Users\usre\.cargo\bin\cargo.exe" build --manifest-path "experiments\p0\Cargo.toml" -p f0c-dwarf --release
& "experiments\p0\target\release\f0c-dwarf.exe" "validation\evidence\f0-c\F0cDwarfFixture.out" "validation\evidence\f0-c\access-plans.json" "D:\SVN\DCU\T26_DCU\trunk\03_code\T26_DCU_APP_NXP\Appl\Output\Exe\T26_DCU_APP_NXP.out"
```

The parser accepts DWARF 4 `.debug_info` references and IAR's
`DW_FORM_ref_sig8` references into DWARF 3/4 `.debug_types`. It only creates
plans for variables with a fixed `DW_OP_addr` location. Flexible arrays require
an explicit slice, and a union selector names the requested member without
inferring an active member.
