# Builds the C++ smoke test against an unpacked release archive, the way a
# customer's project would. CI runs it from ffi-build.yml.
#
#   cmake -S packaging/test -B build -DCMAKE_PREFIX_PATH=<unpacked archive> \
#     -DPOSTHOG_RS_FFI_VERSION=<major.minor> [-DPOSTHOG_RS_FFI_STATIC=ON]
cmake_minimum_required(VERSION 3.16)
project(posthog_rs_ffi_package_test CXX)

find_package(posthog-rs-ffi ${POSTHOG_RS_FFI_VERSION} CONFIG REQUIRED)

add_executable(smoke ../../tests/c/smoke.cpp)
target_compile_features(smoke PRIVATE cxx_std_11)
target_link_libraries(smoke PRIVATE posthog-rs-ffi::posthog-rs-ffi)

if(WIN32 AND NOT POSTHOG_RS_FFI_STATIC)
    # The program needs the DLL next to it.
    add_custom_command(TARGET smoke POST_BUILD
        COMMAND ${CMAKE_COMMAND} -E copy_if_different $<TARGET_FILE:posthog-rs-ffi::shared> $<TARGET_FILE_DIR:smoke>)
endif()
