<template>
  <div class="checkbox">
    <input
      type="checkbox"
      class="checkbox-inn"
      :checked="model"
      @change="model = !model"
    />
    <div class="checkbox-ind">
      <svg
        fill="currentColor"
        aria-hidden="true"
        width="12"
        height="12"
        viewBox="0 0 12 12"
      >
        <path
          d="M9.76 3.2c.3.29.32.76.04 1.06l-4.25 4.5a.75.75 0 0 1-1.08.02L2.22 6.53a.75.75 0 0 1 1.06-1.06l1.7 1.7L8.7 3.24a.75.75 0 0 1 1.06-.04Z"
          fill="currentColor"
        ></path>
      </svg>
    </div>
  </div>
</template>

<script setup lang="ts">
const model = defineModel<boolean>();
</script>
