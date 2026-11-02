require_relative "boot"

require "rails"
require "action_controller/railtie"

Bundler.require(*Rails.groups)

module E2e
  class Application < Rails::Application
    config.load_defaults 8.1
    config.eager_load = true
    config.secret_key_base = "e2e" * 32
    config.logger = ActiveSupport::Logger.new($stdout)
  end
end
