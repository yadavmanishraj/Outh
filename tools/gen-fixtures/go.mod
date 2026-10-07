module outh/gen-fixtures

go 1.27.0

require (
	app v0.0.0
	google.golang.org/protobuf v1.36.12
)

// The gotohp clone is not vendored into the Outh repo; point the module
// replace at the local study clone. Adjust this path if the clone moves —
// see tools/gen-fixtures/README.md.
replace app => /home/hatch/workspace/gotohp-study/gotohp
