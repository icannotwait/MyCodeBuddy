cmake_minimum_required(VERSION 3.20)
project(codeg_eui LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
option(CODEG_EUI_CONTRACTS_ONLY "Build ABI tests without EUI/native deps" OFF)
option(CODEG_EUI_ABI_LINK_TESTS "Link black-box ABI tests to the Rust archive" OFF)
set(CODEG_EUI_RUST_LIB "" CACHE FILEPATH "Absolute libcodeg_eui_core.a path")

enable_testing()

function(codeg_eui_add_contract_test exact_name source)
  set(target "${exact_name}_test")
  add_executable(${target} "${source}" tests/test_main.cpp)
  target_include_directories(${target} PRIVATE tests app app/bridge)
  add_test(NAME "${exact_name}" COMMAND ${target})
endfunction()

codeg_eui_add_contract_test(codeg_eui_harness_self tests/harness_self_test.cpp)
codeg_eui_add_contract_test(codeg_eui_abi_layout tests/abi_layout_test.cpp)
codeg_eui_add_contract_test(codeg_eui_ui_snapshot tests/ui_snapshot_test.cpp)
codeg_eui_add_contract_test(
  codeg_eui_client_completion tests/client_completion_test.cpp)
codeg_eui_add_contract_test(
  codeg_eui_app_lifecycle tests/app_lifecycle_test.cpp)
codeg_eui_add_contract_test(codeg_eui_shell_page tests/shell_page_test.cpp)
codeg_eui_add_contract_test(codeg_eui_chat_page tests/chat_page_test.cpp)
codeg_eui_add_contract_test(
  codeg_eui_settings_page tests/settings_page_test.cpp)
codeg_eui_add_contract_test(
  codeg_eui_m5_controls tests/m5_controls_test.cpp)
codeg_eui_add_contract_test(
  codeg_eui_perf_metrics tests/perf_metrics_test.cpp)

if(CODEG_EUI_ABI_LINK_TESTS OR NOT CODEG_EUI_CONTRACTS_ONLY)
  if(NOT IS_ABSOLUTE "${CODEG_EUI_RUST_LIB}" OR
     NOT EXISTS "${CODEG_EUI_RUST_LIB}")
    message(FATAL_ERROR "CODEG_EUI_RUST_LIB must name an existing absolute archive")
  endif()
  find_package(Threads REQUIRED)
  add_library(codeg_eui_core STATIC IMPORTED GLOBAL)
  set_target_properties(codeg_eui_core PROPERTIES
    IMPORTED_LOCATION "${CODEG_EUI_RUST_LIB}")
endif()

if(CODEG_EUI_ABI_LINK_TESTS)
  codeg_eui_add_contract_test(
    codeg_eui_shutdown_drain tests/shutdown_drain_test.cpp)
  target_compile_definitions(codeg_eui_shutdown_drain_test PRIVATE
    CODEG_EUI_TEST_HOOKS=1)
  target_include_directories(codeg_eui_shutdown_drain_test PRIVATE app/bridge)
  target_link_libraries(codeg_eui_shutdown_drain_test PRIVATE
    codeg_eui_core Threads::Threads ${CMAKE_DL_LIBS} m)
endif()

if(NOT CODEG_EUI_CONTRACTS_ONLY)
  add_subdirectory(third_party/EUI-NEO)
  add_executable(codeg-eui
    third_party/EUI-NEO/core/app/glfw_app_main.cpp
    app/app.cpp)
  target_include_directories(codeg-eui PRIVATE app app/bridge)
  target_link_libraries(codeg-eui PRIVATE
    codeg_eui_core Threads::Threads ${CMAKE_DL_LIBS} m)
  eui_neo_configure_app(codeg-eui)
endif()
