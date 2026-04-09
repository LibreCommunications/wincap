{
  "comment": "Stub binding.gyp — wincap uses CMake (cmake-js). This file exists so npm/node-gyp users get a clear error.",
  "targets": [
    {
      "target_name": "wincap_unsupported_gyp",
      "type": "none",
      "actions": [
        {
          "action_name": "fail",
          "inputs": [],
          "outputs": ["unsupported"],
          "action": ["node", "-e", "console.error('wincap requires cmake-js: run `npx cmake-js compile`'); process.exit(1)"]
        }
      ]
    }
  ]
}
